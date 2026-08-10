/* Files, over RFile.
 *
 * The pleasant part of this platform. RFile has genuine synchronous overloads —
 * Read, Write, Seek and Size all return a TInt directly rather than completing into
 * a TRequestStatus — so unlike sockets and timers there is no active object, no
 * state machine and no event plumbing here. It is a blocking API and Rust can call
 * it as one.
 *
 * Two things still need care.
 *
 * HANDLES, NOT POINTERS. A slot index plus a generation counter, so a stale handle
 * from a use-after-close comes back as SHIM_ERR_BAD_HANDLE instead of writing into a
 * reopened file. Without the generation, closing handle 3 and opening another file
 * would hand out 3 again, and a Rust value that outlived its file would silently
 * address the new one. That is the kind of bug that corrupts a session store and
 * looks like a protocol error a week later.
 *
 * THE DATA CAGE. An unsigned app can write to its own private directory,
 * C:\private\<UID3>\, with no capability at all — it is the one writable location
 * that needs nothing. Anywhere else needs WriteUserData or worse. So
 * shim_private_path is not a convenience: it is where everything this SDK persists
 * has to live.
 */

#include "symbian_shim.h"
#include "shim_priv.h"

#include <e32std.h>
#include <f32file.h>

namespace {

/* Eight is plenty: this exists for a session store, a settings file and a log, not
 * for a database. A fixed table also means no allocation on the open path, so
 * shim_file_open cannot fail for lack of memory. */
const TInt KMaxFiles = 8;

struct TSlot
    {
    RFile iFile;
    TBool iOpen;
    /* Bumped on every close. Handles carry it, so a handle from a previous
     * occupant of this slot no longer validates. */
    TInt iGeneration;
    };

TSlot gSlots[KMaxFiles];
RFs gFs;
TBool gFsOpen = EFalse;

/* A handle packs the slot index in the low 8 bits and the generation above it.
 * Never zero: zero is "no handle" everywhere else in this ABI, so slot 0
 * generation 0 must not collide with it — hence the +1 on the generation. */
inline TInt32 MakeHandle(TInt slot, TInt generation)
    {
    return (TInt32) (((generation + 1) << 8) | (slot & 0xFF));
    }

TSlot* Resolve(int32_t handle)
    {
    if (handle == 0)
        return NULL;
    const TInt slot = handle & 0xFF;
    const TInt generation = ((handle >> 8) & 0xFFFFFF) - 1;
    if (slot < 0 || slot >= KMaxFiles)
        return NULL;
    TSlot& s = gSlots[slot];
    if (!s.iOpen || s.iGeneration != generation)
        return NULL;
    return &s;
    }

TInt FreeSlot()
    {
    for (TInt i = 0; i < KMaxFiles; i++)
        {
        if (!gSlots[i].iOpen)
            return i;
        }
    return KErrNotFound;
    }

/* The file server session, opened on first use.
 *
 * Lazily rather than at startup because a shim that has never touched a file should
 * not hold a session, and because there is no init hook that runs before Rust — the
 * app chain constructs the UI first. Connect() is cheap and idempotent here. */
TInt Fs(RFs*& out)
    {
    if (!gFsOpen)
        {
        const TInt err = gFs.Connect();
        if (err != KErrNone)
            return err;
        /* Shareable across threads in this process.
         *
         * Needed by the image decoder, which runs the codec on a thread the ICL creates
         * (EOptionAlwaysThread, see shim_image.cpp) and hands it this session. A session
         * that is not shared is bound to the thread that connected it, and a codec thread
         * touching it gets a bad-handle panic rather than an error return.
         *
         * The result is deliberately ignored: every other user of this session is
         * single-threaded and works either way, so a refusal here should not stop the app
         * from opening its own files. */
        (void) gFs.ShareAuto();
        gFsOpen = ETrue;
        }
    out = &gFs;
    return KErrNone;
    }

TUint FileMode(int32_t mode)
    {
    TUint m = EFileStream;
    /* EFileShareExclusive: a session store read by two handles at once is a bug we
     * would rather have reported than tolerated. */
    m |= EFileShareExclusive;
    if (mode & SHIM_FILE_WRITE)
        m |= EFileWrite;
    else
        m |= EFileRead;
    return m;
    }

} /* namespace */

/* The one session, for the rest of the shim. Declared in shim_priv.h; the image
 * decoder is the caller. Kept as a thin forward to Fs() so there is still exactly one
 * place that decides when to connect. */
TInt ShimFsSession(RFs*& aOut)
    {
    return Fs(aOut);
    }

extern "C" {

int32_t shim_private_path(uint16_t* buf, int32_t cap, int32_t* len)
    {
    if (!buf || cap <= 0 || !len)
        return SHIM_ERR_ARGUMENT;

    RFs* fs = NULL;
    TInt err = Fs(fs);
    if (err != KErrNone)
        return err;

    TFileName path;
    err = fs->PrivatePath(path);
    if (err != KErrNone)
        return err;

    /* PrivatePath returns a drive-relative path (\private\<uid>\), so the drive has
     * to be prepended by hand. C: on purpose rather than the drive the binary was
     * installed to: a memory card can be removed with the app's data on it, and
     * SessionPath's default is not guaranteed to be writable. */
    TFileName full;
    full.Append(_L("C:"));
    full.Append(path);

    /* Create it if this is the first run. MkDirAll rather than MkDir: the private
     * path has two components and the parent may not exist either. KErrAlreadyExists
     * is the normal case from the second run onwards. */
    err = fs->MkDirAll(full);
    if (err != KErrNone && err != KErrAlreadyExists)
        return err;

    if (full.Length() > cap)
        return SHIM_ERR_OVERFLOW;

    for (TInt i = 0; i < full.Length(); i++)
        buf[i] = full[i];
    *len = full.Length();
    return SHIM_OK;
    }

int32_t shim_file_open(const uint16_t* path, int32_t len, int32_t mode, int32_t* handle)
    {
    if (!path || len <= 0 || !handle)
        return SHIM_ERR_ARGUMENT;
    *handle = 0;

    RFs* fs = NULL;
    TInt err = Fs(fs);
    if (err != KErrNone)
        return err;

    const TInt slot = FreeSlot();
    if (slot == KErrNotFound)
        return SHIM_ERR_IN_USE;

    TPtrC16 name(reinterpret_cast<const TUint16*>(path), len);
    const TUint m = FileMode(mode);
    TSlot& s = gSlots[slot];

    if (mode & SHIM_FILE_APPEND)
        {
        /* Append wins over create, and getting that precedence wrong was a real bug.
         *
         * symbian::fs maps OpenMode::Append to WRITE|CREATE|APPEND, meaning "add to the
         * end, making the file if it is not there". Testing CREATE first turned that
         * into Replace -- which truncates -- so the Seek(ESeekEnd) below landed at zero
         * and an append silently became an overwrite.
         *
         * The host fake did not catch it because it models the OpenMode enum rather than
         * these flags, so its Append branch never went near a truncation. The device self
         * test caught it on the first run. A fake one layer above the layer with the bug
         * cannot see the bug. */
        err = s.iFile.Open(*fs, name, m);
        if (err == KErrNotFound || err == KErrPathNotFound)
            err = s.iFile.Create(*fs, name, m);
        }
    else if (mode & SHIM_FILE_CREATE)
        {
        /* Replace, not Create: Create fails with KErrAlreadyExists, and every caller
         * that asks to create a file it is about to write in full means "make this
         * file be exactly what I am about to write". */
        err = s.iFile.Replace(*fs, name, m);
        }
    else
        {
        err = s.iFile.Open(*fs, name, m);
        }
    if (err != KErrNone)
        return err;

    if (mode & SHIM_FILE_APPEND)
        {
        TInt pos = 0;
        err = s.iFile.Seek(ESeekEnd, pos);
        if (err != KErrNone)
            {
            s.iFile.Close();
            return err;
            }
        }

    s.iOpen = ETrue;
    *handle = MakeHandle(slot, s.iGeneration);
    return SHIM_OK;
    }

int32_t shim_file_read(int32_t handle, uint8_t* buf, int32_t cap, int32_t* got)
    {
    if (!buf || cap <= 0 || !got)
        return SHIM_ERR_ARGUMENT;
    *got = 0;
    TSlot* s = Resolve(handle);
    if (!s)
        return SHIM_ERR_BAD_HANDLE;

    /* TPtr8 over Rust's buffer: no copy, and RFile writes straight into it. The
     * descriptor's max length is what caps the read, which is why cap must be the
     * real capacity and not a length. */
    TPtr8 des(reinterpret_cast<TUint8*>(buf), 0, cap);
    const TInt err = s->iFile.Read(des);
    if (err != KErrNone)
        return err;
    /* A short read is not an error and end-of-file is a zero-length read, so the
     * caller distinguishes them by `got`, not by the return value. */
    *got = des.Length();
    return SHIM_OK;
    }

int32_t shim_file_write(int32_t handle, const uint8_t* buf, int32_t len)
    {
    if (!buf || len < 0)
        return SHIM_ERR_ARGUMENT;
    if (len == 0)
        return SHIM_OK;
    TSlot* s = Resolve(handle);
    if (!s)
        return SHIM_ERR_BAD_HANDLE;

    TPtrC8 des(reinterpret_cast<const TUint8*>(buf), len);
    return s->iFile.Write(des);
    }

int32_t shim_file_size(int32_t handle, int64_t* out)
    {
    if (!out)
        return SHIM_ERR_ARGUMENT;
    *out = 0;
    TSlot* s = Resolve(handle);
    if (!s)
        return SHIM_ERR_BAD_HANDLE;

    TInt size = 0;
    const TInt err = s->iFile.Size(size);
    if (err != KErrNone)
        return err;
    *out = size;
    return SHIM_OK;
    }

int32_t shim_file_seek(int32_t handle, int64_t pos)
    {
    TSlot* s = Resolve(handle);
    if (!s)
        return SHIM_ERR_BAD_HANDLE;
    if (pos < 0)
        return SHIM_ERR_ARGUMENT;
    /* Symbian 9.3's RFile is 32-bit: the 64-bit RFile64 arrived in Symbian^3. The ABI
     * takes int64 anyway so it does not need changing when that becomes reachable,
     * but a position past 2 GB has to be refused rather than silently truncated. */
    if (pos > KMaxTInt)
        return SHIM_ERR_OVERFLOW;

    TInt p = (TInt) pos;
    return s->iFile.Seek(ESeekStart, p);
    }

int32_t shim_file_delete(const uint16_t* path, int32_t len)
    {
    if (!path || len <= 0)
        return SHIM_ERR_ARGUMENT;
    RFs* fs = NULL;
    const TInt err = Fs(fs);
    if (err != KErrNone)
        return err;
    TPtrC16 name(reinterpret_cast<const TUint16*>(path), len);
    return fs->Delete(name);
    }

int32_t shim_file_rename(const uint16_t* from, int32_t from_len,
                         const uint16_t* to, int32_t to_len)
    {
    if (!from || from_len <= 0 || !to || to_len <= 0)
        return SHIM_ERR_ARGUMENT;
    RFs* fs = NULL;
    const TInt err = Fs(fs);
    if (err != KErrNone)
        return err;

    TPtrC16 src(reinterpret_cast<const TUint16*>(from), from_len);
    TPtrC16 dst(reinterpret_cast<const TUint16*>(to), to_len);

    /* RFs::Rename fails with KErrAlreadyExists rather than replacing, so the
     * destination goes first. That opens a window where neither name holds the new
     * data — but the *old* file is still intact until the rename lands, so a crash
     * in the window loses the update rather than corrupting it. Losing an update is
     * recoverable; a half-written store is not.
     *
     * KErrNotFound from the delete is the normal first-save case. */
    const TInt derr = fs->Delete(dst);
    if (derr != KErrNone && derr != KErrNotFound)
        return derr;

    return fs->Rename(src, dst);
    }

int32_t shim_mkdir(const uint16_t* path, int32_t path_len)
    {
    if (!path || path_len <= 0)
        return SHIM_ERR_ARGUMENT;
    RFs* fs = NULL;
    const TInt err = Fs(fs);
    if (err != KErrNone)
        return err;
    TPtrC16 p(reinterpret_cast<const TUint16*>(path), path_len);
    /* MkDirAll creates every missing component. An existing directory is success as far
     * as the caller cares — it wanted the directory to be there, and it is. */
    const TInt rc = fs->MkDirAll(p);
    return (rc == KErrNone || rc == KErrAlreadyExists) ? SHIM_OK : rc;
    }

int32_t shim_dir_list(const uint16_t* path, int32_t path_len, uint16_t* buf, int32_t cap, int32_t* count)
    {
    if (count)
        *count = 0;
    if (!path || path_len <= 0 || !buf || cap <= 0)
        return SHIM_ERR_ARGUMENT;
    RFs* fs = NULL;
    const TInt err = Fs(fs);
    if (err != KErrNone)
        return err;

    TPtrC16 dir(reinterpret_cast<const TUint16*>(path), path_len);
    CDir* entries = NULL;
    /* Synchronous listing; files only via the sort/attribute filter below. */
    const TInt gerr = fs->GetDir(dir, KEntryAttNormal | KEntryAttHidden, ESortByName, entries);
    if (gerr != KErrNone)
        {
        /* A directory that is not there yet is not a failure — nothing to list. */
        return (gerr == KErrNotFound || gerr == KErrPathNotFound) ? SHIM_OK : gerr;
        }

    TInt pos = 0;    /* units written into buf */
    TInt n = 0;
    const TInt total = entries->Count();
    for (TInt i = 0; i < total; i++)
        {
        const TEntry& e = (*entries)[i];
        if (e.IsDir())
            continue;
        const TDesC& name = e.iName;
        /* name + a NUL separator must fit, or stop and report what did. */
        if (pos + name.Length() + 1 > cap)
            break;
        for (TInt j = 0; j < name.Length(); j++)
            buf[pos++] = name[j];
        buf[pos++] = 0;
        n++;
        }
    delete entries;
    if (count)
        *count = n;
    return SHIM_OK;
    }

void shim_file_close(int32_t handle)
    {
    TSlot* s = Resolve(handle);
    if (!s)
        return;
    s->iFile.Close();
    s->iOpen = EFalse;
    /* Bump before reuse, so the handle just closed can never validate again. Wraps
     * at 24 bits, which after 16 million opens of the same slot is a theoretical
     * collision this code is not going to reach. */
    s->iGeneration = (s->iGeneration + 1) & 0xFFFFFF;
    }

} /* extern "C" */

/* Called from the app's teardown. Every slot, then the session: a leaked RFile keeps
 * a file server handle open past process exit on some builds, and the panic that
 * causes points at the file server rather than at us. */
void ShimFilesCleanup()
    {
    for (TInt i = 0; i < KMaxFiles; i++)
        {
        if (gSlots[i].iOpen)
            {
            gSlots[i].iFile.Close();
            gSlots[i].iOpen = EFalse;
            }
        }
    if (gFsOpen)
        {
        gFs.Close();
        gFsOpen = EFalse;
        }
    }
