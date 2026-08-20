/* recogprobe — an ECom data-recogniser that recognises nothing and only records that it ran.
 *
 * ECom loads this DLL (interface 0x101F7D87, from recogprobe.rss in \resource\plugins), calls
 * ImplementationGroupProxy at ordinal 1 for the factory table, then calls the factory to build
 * our recogniser. ECOM_RECOG_EXPORT wires that and calls OnLoad() on the fresh object. OnLoad
 * appends one line with two boot clocks to C:\Data\recog_probe.txt, so reading it after a real
 * reboot answers Phase A': does the ECom recogniser factory run at boot, and how early?
 *
 * Non-leaving, never panics: it runs inside the AppArc/ECom server. A failed open/write is
 * swallowed — an empty file is itself a (negative) result, not a crash in a system process.
 */

#include <e32std.h>
#include <f32file.h>
#include "recogbase.h"

_LIT(KProbeDir,  "C:\\Data\\");
_LIT(KProbePath, "C:\\Data\\recog_probe.txt");

class CRecogProbe : public CRecogBase
    {
public:
    CRecogProbe() : CRecogBase() {}
    void OnLoad();
    };

void CRecogProbe::OnLoad()
    {
    RFs fs;
    if (fs.Connect() != KErrNone)
        return;
    fs.MkDirAll(KProbeDir);

    RFile file;
    TInt rc = file.Open(fs, KProbePath, EFileWrite | EFileShareAny);
    if (rc == KErrNotFound || rc == KErrPathNotFound)
        rc = file.Create(fs, KProbePath, EFileWrite | EFileShareAny);
    if (rc == KErrNone)
        {
        TInt pos = 0;
        file.Seek(ESeekEnd, pos);
        TBuf8<96> line;
        line.Append(_L8("boot ntick="));
        line.AppendNum((TInt64)User::NTickCount());
        line.Append(_L8(" tick="));
        line.AppendNum((TInt64)User::TickCount());
        line.Append(_L8("\r\n"));
        file.Write(line);
        file.Flush();
        file.Close();
        }
    fs.Close();
    }

/* UID3 = implementation_uid = the .rss's implementation_uid. Must match recogprobe.rss. */
ECOM_RECOG_EXPORT(CRecogProbe, 0xE0DD00F3)
