/* Symbian SQL, over RSqlDatabase and RSqlStatement.
 *
 * The platform ships SQLite as a server (sqldb.dll) and this is the client side of it.
 * Like RFile and unlike sockets, the API is genuinely synchronous — every operation
 * returns a TInt rather than completing into a TRequestStatus — so there is no active
 * object here, no state machine and no event plumbing.
 *
 * WHY THERE IS NO TRAP IN THIS FILE. Every entry point calls the non-leaving overload:
 * Open rather than OpenL, Prepare rather than PrepareL, Exec rather than ExecL. Those
 * are implemented on the platform side as a TRAP around the leaving variant and return
 * the error as a TInt, so there is nothing left for a TRAP of ours to catch. The header
 * says this too, because a reader who knows rule 1 will otherwise look for the barrier
 * and wonder what was forgotten.
 *
 * HANDLES, NOT POINTERS, and the same slot-plus-generation scheme as shim_file.cpp.
 * There are two tables — databases and statements — and a statement records which
 * database slot it belongs to. That link is what makes shim_sql_close able to finalise
 * the statements still open on a database it is about to close, which matters because a
 * statement outliving its database is a handle into freed server-side state and the
 * panic that follows names the SQL server rather than us.
 *
 * TWO DATABASES, EIGHT STATEMENTS. Fixed tables so nothing allocates on the open path.
 * Two databases because the shape this exists for is one store plus, briefly, a second
 * one being migrated into it; eight statements because a prepared statement is worth
 * keeping for the lifetime of a screen and no screen has more than a handful of queries.
 * ATTACH is the answer for anything wider, and it costs no slot.
 */

#include "symbian_shim.h"
#include "shim_priv.h"

#include <e32std.h>
#include <sqldb.h>

namespace {

const TInt KMaxDbs = 2;
const TInt KMaxStmts = 8;

struct TDbSlot
    {
    RSqlDatabase iDb;
    TBool iOpen;
    TInt iGeneration;
    };

struct TStmtSlot
    {
    RSqlStatement iStmt;
    TBool iOpen;
    TInt iGeneration;
    /* Which database slot this statement was prepared against, so closing that
     * database can finalise it first. */
    TInt iDb;
    };

TDbSlot gDbs[KMaxDbs];
TStmtSlot gStmts[KMaxStmts];

/* Same packing as the file table: slot index in the low 8 bits, generation above it,
 * never zero. The two tables are numbered independently, so a database handle passed
 * where a statement handle belongs resolves against the wrong table — which is why
 * both Resolve functions check the generation rather than only the range. */
inline TInt32 MakeHandle(TInt aSlot, TInt aGeneration)
    {
    return (TInt32) (((aGeneration + 1) << 8) | (aSlot & 0xFF));
    }

inline TInt SlotOf(int32_t aHandle)
    {
    return aHandle & 0xFF;
    }

inline TInt GenerationOf(int32_t aHandle)
    {
    return ((aHandle >> 8) & 0xFFFFFF) - 1;
    }

TDbSlot* ResolveDb(int32_t aHandle)
    {
    if (aHandle == 0)
        return NULL;
    const TInt slot = SlotOf(aHandle);
    if (slot < 0 || slot >= KMaxDbs)
        return NULL;
    TDbSlot& s = gDbs[slot];
    if (!s.iOpen || s.iGeneration != GenerationOf(aHandle))
        return NULL;
    return &s;
    }

TStmtSlot* ResolveStmt(int32_t aHandle)
    {
    if (aHandle == 0)
        return NULL;
    const TInt slot = SlotOf(aHandle);
    if (slot < 0 || slot >= KMaxStmts)
        return NULL;
    TStmtSlot& s = gStmts[slot];
    if (!s.iOpen || s.iGeneration != GenerationOf(aHandle))
        return NULL;
    return &s;
    }

void CloseStmtSlot(TStmtSlot& aSlot)
    {
    aSlot.iStmt.Close();
    aSlot.iOpen = EFalse;
    aSlot.iDb = -1;
    aSlot.iGeneration = (aSlot.iGeneration + 1) & 0xFFFFFF;
    }

TInt FreeDb()
    {
    for (TInt i = 0; i < KMaxDbs; i++)
        {
        if (!gDbs[i].iOpen)
            return i;
        }
    return KErrNotFound;
    }

TInt FreeStmt()
    {
    for (TInt i = 0; i < KMaxStmts; i++)
        {
        if (!gStmts[i].iOpen)
            return i;
        }
    return KErrNotFound;
    }

/* KSqlAtRow / KSqlAtEnd are positive returns from Next() and everything negative is an
 * error. Translated here so the flat ABI carries SHIM_SQL_ROW / SHIM_SQL_DONE and Rust
 * never learns the platform's names for them. */
int32_t TranslateStep(TInt aRc)
    {
    if (aRc == KSqlAtRow)
        return SHIM_SQL_ROW;
    if (aRc == KSqlAtEnd)
        return SHIM_SQL_DONE;
    return aRc;
    }

/* Copy a descriptor out to the caller, reporting the full length either way.
 *
 * The length is written before the capacity test on purpose: a caller that got
 * SHIM_ERR_OVERFLOW needs to know how much to allocate, and making it call again to find
 * out would double the round trips on exactly the path that is already the slow one. */
int32_t CopyOut16(const TDesC& aSrc, uint16_t* aBuf, int32_t aCap, int32_t* aLen)
    {
    *aLen = aSrc.Length();
    if (aSrc.Length() > aCap)
        return SHIM_ERR_OVERFLOW;
    for (TInt i = 0; i < aSrc.Length(); i++)
        aBuf[i] = aSrc[i];
    return SHIM_OK;
    }

} /* namespace */

extern "C" {

int32_t shim_sql_open(const uint16_t* path, int32_t path_len, int32_t create, int32_t* handle)
    {
    if (!path || path_len <= 0 || !handle)
        return SHIM_ERR_ARGUMENT;
    *handle = 0;

    const TInt slot = FreeDb();
    if (slot == KErrNotFound)
        return SHIM_ERR_IN_USE;

    TPtrC16 name(reinterpret_cast<const TUint16*>(path), path_len);
    TDbSlot& s = gDbs[slot];

    /* Open first, create only if it is genuinely absent — the same precedence as
     * shim_file_open's append, and for the same reason. Create() on an existing
     * database fails with KErrAlreadyExists rather than reopening it, so leading with
     * Create would make the second run of every app an error. */
    TInt err = s.iDb.Open(name);
    if (err == KErrNotFound && create)
        {
        /* The non-secure variant: no RSqlSecurityPolicy, so no capability is needed and
         * the file is an ordinary one under the app's private path. A secure database
         * would key its policy on this process's SID and be unreadable from the dev
         * bridge, which is the opposite of what a store being debugged wants. */
        err = s.iDb.Create(name);
        }
    if (err != KErrNone)
        return err;

    s.iOpen = ETrue;
    *handle = MakeHandle(slot, s.iGeneration);
    return SHIM_OK;
    }

void shim_sql_close(int32_t db)
    {
    TDbSlot* s = ResolveDb(db);
    if (!s)
        return;
    const TInt slot = SlotOf(db);

    /* Statements before the database. A statement holds server-side state that belongs
     * to this connection; closing the connection under it leaves a handle whose own
     * close panics, and the panic names the SQL server. */
    for (TInt i = 0; i < KMaxStmts; i++)
        {
        if (gStmts[i].iOpen && gStmts[i].iDb == slot)
            CloseStmtSlot(gStmts[i]);
        }

    s->iDb.Close();
    s->iOpen = EFalse;
    s->iGeneration = (s->iGeneration + 1) & 0xFFFFFF;
    }

int32_t shim_sql_delete(const uint16_t* path, int32_t path_len)
    {
    if (!path || path_len <= 0)
        return SHIM_ERR_ARGUMENT;
    TPtrC16 name(reinterpret_cast<const TUint16*>(path), path_len);
    /* Static, so it needs no open connection — and must not have one: deleting a
     * database another handle still holds open fails with KErrInUse. */
    return RSqlDatabase::Delete(name);
    }

int32_t shim_sql_exec(int32_t db, const uint8_t* sql, int32_t len, int32_t* changed)
    {
    if (changed)
        *changed = 0;
    if (!sql || len <= 0)
        return SHIM_ERR_ARGUMENT;
    TDbSlot* s = ResolveDb(db);
    if (!s)
        return SHIM_ERR_BAD_HANDLE;

    /* The 8-bit overload: the statement text is UTF-8, which is what Rust already holds.
     * TPtrC8 wraps it with no copy. */
    TPtrC8 stmt(reinterpret_cast<const TUint8*>(sql), len);
    const TInt rc = s->iDb.Exec(stmt);
    /* Exec returns the number of rows changed on success, so a positive value is not an
     * error — a distinction worth being explicit about, because every other function in
     * this ABI returns 0 for success. */
    if (rc < 0)
        return rc;
    if (changed)
        *changed = rc;
    return SHIM_OK;
    }

int32_t shim_sql_size(int32_t db, int32_t* out)
    {
    if (!out)
        return SHIM_ERR_ARGUMENT;
    *out = 0;
    TDbSlot* s = ResolveDb(db);
    if (!s)
        return SHIM_ERR_BAD_HANDLE;
    const TInt rc = s->iDb.Size();
    if (rc < 0)
        return rc;
    *out = rc;
    return SHIM_OK;
    }

int32_t shim_sql_last_error(int32_t db, uint16_t* buf, int32_t cap, int32_t* len)
    {
    if (!buf || cap <= 0 || !len)
        return SHIM_ERR_ARGUMENT;
    *len = 0;
    TDbSlot* s = ResolveDb(db);
    if (!s)
        return SHIM_ERR_BAD_HANDLE;
    /* Points into the connection's own buffer and stays valid only until the next
     * operation on it, which is why it is copied out here and not handed over. */
    const TPtrC msg = s->iDb.LastErrorMessage();
    return CopyOut16(msg, buf, cap, len);
    }

int32_t shim_sql_prepare(int32_t db, const uint8_t* sql, int32_t len, int32_t* stmt)
    {
    if (!sql || len <= 0 || !stmt)
        return SHIM_ERR_ARGUMENT;
    *stmt = 0;
    TDbSlot* d = ResolveDb(db);
    if (!d)
        return SHIM_ERR_BAD_HANDLE;

    const TInt slot = FreeStmt();
    if (slot == KErrNotFound)
        return SHIM_ERR_IN_USE;

    TPtrC8 text(reinterpret_cast<const TUint8*>(sql), len);
    TStmtSlot& s = gStmts[slot];
    const TInt err = s.iStmt.Prepare(d->iDb, text);
    if (err != KErrNone)
        return err;

    s.iOpen = ETrue;
    s.iDb = SlotOf(db);
    *stmt = MakeHandle(slot, s.iGeneration);
    return SHIM_OK;
    }

void shim_sql_finalize(int32_t stmt)
    {
    TStmtSlot* s = ResolveStmt(stmt);
    if (!s)
        return;
    CloseStmtSlot(*s);
    }

int32_t shim_sql_reset(int32_t stmt)
    {
    TStmtSlot* s = ResolveStmt(stmt);
    if (!s)
        return SHIM_ERR_BAD_HANDLE;
    return s->iStmt.Reset();
    }

int32_t shim_sql_step(int32_t stmt)
    {
    TStmtSlot* s = ResolveStmt(stmt);
    if (!s)
        return SHIM_ERR_BAD_HANDLE;
    return TranslateStep(s->iStmt.Next());
    }

int32_t shim_sql_exec_stmt(int32_t stmt, int32_t* changed)
    {
    if (changed)
        *changed = 0;
    TStmtSlot* s = ResolveStmt(stmt);
    if (!s)
        return SHIM_ERR_BAD_HANDLE;
    /* Exec, not Next, and the distinction is not stylistic: Next() on a statement that
     * produces no row set panics, which closes the process. examples/sqlprobe found that by
     * stepping a prepared INSERT -- the breadcrumb in its report reads `-> step` and then
     * the file ends.
     *
     * Returns the number of rows changed, so a positive value is success. */
    const TInt rc = s->iStmt.Exec();
    if (rc < 0)
        return rc;
    if (changed)
        *changed = rc;
    return SHIM_OK;
    }

int32_t shim_sql_bind_null(int32_t stmt, int32_t index)
    {
    TStmtSlot* s = ResolveStmt(stmt);
    if (!s)
        return SHIM_ERR_BAD_HANDLE;
    return s->iStmt.BindNull(index);
    }

int32_t shim_sql_bind_int(int32_t stmt, int32_t index, int64_t value)
    {
    TStmtSlot* s = ResolveStmt(stmt);
    if (!s)
        return SHIM_ERR_BAD_HANDLE;
    /* Always the 64-bit bind, even for a value that would fit a TInt. SQLite stores an
     * integer in as few bytes as it needs regardless, so the narrower overload buys
     * nothing and would make the ABI carry two entry points where one does. */
    return s->iStmt.BindInt64(index, value);
    }

int32_t shim_sql_bind_real(int32_t stmt, int32_t index, double value)
    {
    TStmtSlot* s = ResolveStmt(stmt);
    if (!s)
        return SHIM_ERR_BAD_HANDLE;
    return s->iStmt.BindReal(index, value);
    }

int32_t shim_sql_bind_text(int32_t stmt, int32_t index, const uint16_t* text, int32_t len)
    {
    if (!text || len < 0)
        return SHIM_ERR_ARGUMENT;
    TStmtSlot* s = ResolveStmt(stmt);
    if (!s)
        return SHIM_ERR_BAD_HANDLE;
    /* The descriptor must outlive the bind but not the step: BindText copies into the
     * statement's parameter buffer, so the caller's buffer is free again on return.
     * (BindTextL's streaming variant is the one that does not, and is not used here.) */
    TPtrC16 t(reinterpret_cast<const TUint16*>(text), len);
    return s->iStmt.BindText(index, t);
    }

int32_t shim_sql_bind_blob(int32_t stmt, int32_t index, const uint8_t* data, int32_t len)
    {
    if (!data || len < 0)
        return SHIM_ERR_ARGUMENT;
    TStmtSlot* s = ResolveStmt(stmt);
    if (!s)
        return SHIM_ERR_BAD_HANDLE;
    TPtrC8 d(reinterpret_cast<const TUint8*>(data), len);
    return s->iStmt.BindBinary(index, d);
    }

int32_t shim_sql_column_type(int32_t stmt, int32_t col, int32_t* out)
    {
    if (!out)
        return SHIM_ERR_ARGUMENT;
    *out = SHIM_SQL_NULL;
    TStmtSlot* s = ResolveStmt(stmt);
    if (!s)
        return SHIM_ERR_BAD_HANDLE;

    switch (s->iStmt.ColumnType(col))
        {
        case ESqlNull:   *out = SHIM_SQL_NULL; break;
        /* Int and Int64 collapse into one: the difference is a storage detail of the
         * row buffer and Rust reads both through ColumnInt64 below. */
        case ESqlInt:
        case ESqlInt64:  *out = SHIM_SQL_INT;  break;
        case ESqlReal:   *out = SHIM_SQL_REAL; break;
        case ESqlText:   *out = SHIM_SQL_TEXT; break;
        case ESqlBinary: *out = SHIM_SQL_BLOB; break;
        /* Unreachable on the handset, and kept anyway.
         *
         * The intent was to catch an out-of-range column index and report it as an
         * argument error. It cannot: the platform asserts on a bad index and the process
         * dies before this switch is reached — examples/sqlprobe proved that. So this
         * branch only ever fires if a future platform adds a column type this ABI has no
         * value for, which is the other thing it would be wrong to pass off as NULL. */
        default: return SHIM_ERR_ARGUMENT;
        }
    return SHIM_OK;
    }

int32_t shim_sql_column_int(int32_t stmt, int32_t col, int64_t* out)
    {
    if (!out)
        return SHIM_ERR_ARGUMENT;
    *out = 0;
    TStmtSlot* s = ResolveStmt(stmt);
    if (!s)
        return SHIM_ERR_BAD_HANDLE;
    *out = s->iStmt.ColumnInt64(col);
    return SHIM_OK;
    }

int32_t shim_sql_column_real(int32_t stmt, int32_t col, double* out)
    {
    if (!out)
        return SHIM_ERR_ARGUMENT;
    *out = 0.0;
    TStmtSlot* s = ResolveStmt(stmt);
    if (!s)
        return SHIM_ERR_BAD_HANDLE;
    *out = s->iStmt.ColumnReal(col);
    return SHIM_OK;
    }

int32_t shim_sql_column_text(int32_t stmt, int32_t col, uint16_t* buf, int32_t cap, int32_t* len)
    {
    if (!buf || cap < 0 || !len)
        return SHIM_ERR_ARGUMENT;
    *len = 0;
    TStmtSlot* s = ResolveStmt(stmt);
    if (!s)
        return SHIM_ERR_BAD_HANDLE;

    /* The pointer overload rather than the copying one, so the length can be reported
     * even when the caller's buffer is too small. The TPtrC points into the row buffer
     * and is valid until the next step or reset, which is inside this call. */
    TPtrC ptr;
    const TInt err = s->iStmt.ColumnText(col, ptr);
    if (err != KErrNone)
        return err;
    return CopyOut16(ptr, buf, cap, len);
    }

int32_t shim_sql_column_blob(int32_t stmt, int32_t col, uint8_t* buf, int32_t cap, int32_t* len)
    {
    if (!buf || cap < 0 || !len)
        return SHIM_ERR_ARGUMENT;
    *len = 0;
    TStmtSlot* s = ResolveStmt(stmt);
    if (!s)
        return SHIM_ERR_BAD_HANDLE;

    TPtrC8 ptr;
    const TInt err = s->iStmt.ColumnBinary(col, ptr);
    if (err != KErrNone)
        return err;

    *len = ptr.Length();
    if (ptr.Length() > cap)
        return SHIM_ERR_OVERFLOW;
    for (TInt i = 0; i < ptr.Length(); i++)
        buf[i] = ptr[i];
    return SHIM_OK;
    }

int32_t shim_sql_column_index(int32_t stmt, const uint16_t* name, int32_t len, int32_t* out)
    {
    if (!name || len <= 0 || !out)
        return SHIM_ERR_ARGUMENT;
    *out = -1;
    TStmtSlot* s = ResolveStmt(stmt);
    if (!s)
        return SHIM_ERR_BAD_HANDLE;

    TPtrC16 n(reinterpret_cast<const TUint16*>(name), len);
    const TInt rc = s->iStmt.ColumnIndex(n);
    if (rc < 0)
        return rc;
    *out = rc;
    return SHIM_OK;
    }

} /* extern "C" */

/* Called from the app's teardown, before ShimFilesCleanup for no reason of ordering —
 * the SQL server holds its own file handles, not ours — but kept alongside it so there
 * is one place where every facility that owns a kernel handle is released.
 *
 * Statements first, then databases, for the reason shim_sql_close documents. */
void ShimSqlCleanup()
    {
    for (TInt i = 0; i < KMaxStmts; i++)
        {
        if (gStmts[i].iOpen)
            CloseStmtSlot(gStmts[i]);
        }
    for (TInt i = 0; i < KMaxDbs; i++)
        {
        if (gDbs[i].iOpen)
            {
            gDbs[i].iDb.Close();
            gDbs[i].iOpen = EFalse;
            }
        }
    }
