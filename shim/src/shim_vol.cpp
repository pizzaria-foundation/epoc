/* What is mounted, what kind of medium it is, and how much room is on it.
 *
 * Three calls because the platform asks three questions and keeps them apart:
 *
 *   RFs::DriveList   which drive letters exist at all
 *   RFs::Drive       what kind of thing each one is
 *   RFs::Volume      how big it is and how much is free
 *
 * Merging them would hide the case that matters most. A drive can be *present* with no
 * volume *mounted* — an empty memory-card slot is exactly that — and RFs::Volume answers
 * KErrNotReady for it while RFs::Drive answers happily. A single merged call would have to
 * pick one of those to report, and would turn "no card inserted" into either "no drive" or
 * "a drive of size zero", both of which are wrong in a way that reads as fact.
 *
 * No TRAP anywhere in this file: every RFs method used here returns TInt and none of them
 * Leaves. Stated rather than assumed, per rule 1 in symbian_shim.h.
 */

#include "symbian_shim.h"
#include "shim_priv.h"

#ifdef SHIM_USE_FS_INFO

#include <e32std.h>
#include <f32file.h>

extern "C" {

int32_t shim_drive_list(uint32_t* out_mask)
    {
    if (!out_mask)
        return SHIM_ERR_ARGUMENT;

    /* A session of its own rather than the shim's shared one: this file is compiled into
     * probe binaries that may not link the rest of the shim at all, and a drive listing
     * that depended on the file subsystem being initialised would be one more way for a
     * diagnostic to fail for a reason unrelated to what it measures. */
    RFs fs;
    TInt rc = fs.Connect();
    if (rc != KErrNone)
        return rc;

    TDriveList list;
    rc = fs.DriveList(list);
    if (rc == KErrNone)
        {
        TUint32 mask = 0;
        /* TDriveList is KMaxDrives bytes, one per letter, nonzero meaning present. */
        const TInt n = list.Length() < 26 ? list.Length() : 26;
        for (TInt i = 0; i < n; i++)
            {
            if (list[i])
                mask |= (TUint32) 1u << i;
            }
        *out_mask = mask;
        }
    fs.Close();
    return rc == KErrNone ? SHIM_OK : rc;
    }

int32_t shim_drive_info(int32_t drive, ShimDriveInfo* out)
    {
    if (!out || drive < 0 || drive > 25)
        return SHIM_ERR_ARGUMENT;

    RFs fs;
    TInt rc = fs.Connect();
    if (rc != KErrNone)
        return rc;

    TDriveInfo info;
    rc = fs.Drive(info, (TInt) drive);
    if (rc == KErrNone)
        {
        out->type = (int32_t) info.iType;
        out->battery = (int32_t) info.iBattery;
        out->drive_att = (uint32_t) info.iDriveAtt;
        out->media_att = (uint32_t) info.iMediaAtt;
        }
    fs.Close();
    return rc == KErrNone ? SHIM_OK : rc;
    }

int32_t shim_volume_info(int32_t drive, ShimVolumeInfo* out)
    {
    if (!out || drive < 0 || drive > 25)
        return SHIM_ERR_ARGUMENT;

    RFs fs;
    TInt rc = fs.Connect();
    if (rc != KErrNone)
        return rc;

    TVolumeInfo info;
    rc = fs.Volume(info, (TInt) drive);
    if (rc == KErrNone)
        {
        out->size = (int64_t) info.iSize;
        out->free = (int64_t) info.iFree;
        out->unique_id = (uint32_t) info.iUniqueID;

        const TInt cap = (TInt) (sizeof(out->name) / sizeof(out->name[0]));
        TInt n = info.iName.Length();
        if (n > cap)
            n = cap;
        for (TInt i = 0; i < n; i++)
            out->name[i] = (uint16_t) info.iName[i];
        out->name_len = (int32_t) n;
        }
    fs.Close();
    /* KErrNotReady reaches the caller unchanged: a present drive with nothing mounted is
     * a finding about the handset, not a failure of this call. */
    return rc == KErrNone ? SHIM_OK : rc;
    }

} /* extern "C" */

#endif /* SHIM_USE_FS_INFO */
