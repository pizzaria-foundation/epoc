/* recoglaunch — an ECom data-recogniser whose side effect is to LAUNCH THE LAUNCHER early in boot.
 *
 * Same shape as recogprobe (recognises nothing; ECom instantiates it ~3.7 s into boot, proven by
 * the probe). Here OnLoad does the real work: if the launcher is not already up, start
 * \sys\bin\launcher.exe with RProcess::Create. RProcess, not RApaLsSession::StartApp, because we
 * run inside the AppArc/ECom server and must not re-enter it. A guard (TFindProcess) makes the
 * launch idempotent — ECom may construct the recogniser more than once, and the launcher, once up,
 * is the home screen and stays up.
 *
 * Non-leaving, never panics: it runs inside a system server. Every step logs to
 * C:\Data\recog_launch.txt so a failed launch (e.g. the window server not yet ready this early)
 * is diagnosable rather than silent.
 */

#include <e32std.h>
#include <f32file.h>
#include <e32property.h>   /* RProperty — the atomic once-per-boot latch */
#include "recogbase.h"

_LIT(KDir,       "C:\\Data\\");
_LIT(KLog,       "C:\\Data\\recog_launch.txt");
_LIT(KLauncher,  "launcher.exe");          /* loader searches \sys\bin */
_LIT(KMatch,     "*launcher*");            /* TFindProcess pattern over full process names */
_LIT(KFlag,      "C:\\Data\\replace_main.flag");

/* Replace Main gate. The launcher publishes C:\Data\replace_main.flag: a single byte "0" means
 * the user turned Replace Main off, so we must NOT take over the boot. An absent file or "1" means
 * go (matching the launcher's resident default of on). Read-only, best effort — if we cannot tell,
 * default to launching. */
static TBool ReplaceMainEnabled()
    {
    RFs fs;
    if (fs.Connect() != KErrNone)
        return ETrue;
    TBool enabled = ETrue;
    RFile f;
    if (f.Open(fs, KFlag, EFileRead | EFileShareAny) == KErrNone)
        {
        TBuf8<4> buf;
        if (f.Read(buf, 1) == KErrNone && buf.Length() >= 1 && buf[0] == '0')
            enabled = EFalse;
        f.Close();
        }
    fs.Close();
    return enabled;
    }

class CRecogLaunch : public CRecogBase
    {
public:
    CRecogLaunch() : CRecogBase() {}
    void OnLoad();
    };

void CRecogLaunch::OnLoad()
    {
    /* Guard 0 — Replace Main. If the user turned it off, the launcher must not hijack the boot. */
    if (!ReplaceMainEnabled())
        return;

    /* Guard 1 — already running? Catches the case where the launcher is up (a later recognition,
     * or a second boot cycle). Process full names look like "launcher.exe[e0aa0000]0001". But at
     * boot the ECom factory is called several times within ~10 ms, and a just-Created process is
     * not yet findable, so this alone let three launches through (measured). */
    TFindProcess finder(KMatch);
    TFullName fn;
    TBool already = (finder.Next(fn) == KErrNone);

    /* Guard 2 — an atomic once-per-boot latch. A Publish & Subscribe property is volatile (it is
     * cleared on every boot), so RProperty::Define is a test-and-set: the FIRST caller of the
     * boot-time burst gets KErrNone and launches; the rest get KErrAlreadyExists and skip. The
     * category is this process's own SID (apparc's), so defining needs no capability, and the
     * burst all runs in that one process, so they share the latch. Next boot: the property is
     * gone and the latch re-arms by itself. */
    const TUid latchCat = { RProcess().SecureId().iId };
    const TUint latchKey = 0xE0DD00F4;
    const TInt latch = RProperty::Define(latchCat, latchKey, RProperty::EInt);
    const TBool weAreFirst = (latch == KErrNone);   /* KErrAlreadyExists => someone already latched */

    TInt createErr = KErrNotReady;   /* sentinel: no launch attempted */
    if (!already && weAreFirst)
        {
        RProcess proc;
        createErr = proc.Create(KLauncher, KNullDesC);
        if (createErr == KErrNone)
            {
            proc.Resume();
            proc.Close();
            }
        }

    /* Log uptime + outcome (best effort — a logging failure must not disturb the server). */
    RFs fs;
    if (fs.Connect() != KErrNone)
        return;
    fs.MkDirAll(KDir);
    RFile file;
    TInt rc = file.Open(fs, KLog, EFileWrite | EFileShareAny);
    if (rc == KErrNotFound || rc == KErrPathNotFound)
        rc = file.Create(fs, KLog, EFileWrite | EFileShareAny);
    if (rc == KErrNone)
        {
        TInt pos = 0;
        file.Seek(ESeekEnd, pos);
        TBuf8<128> line;
        line.Append(_L8("boot ntick="));
        line.AppendNum((TInt64)User::NTickCount());
        line.Append(_L8(" tick="));
        line.AppendNum((TInt64)User::TickCount());
        line.Append(_L8(" already="));
        line.AppendNum(already ? 1 : 0);
        line.Append(_L8(" latch="));
        line.AppendNum(latch);
        line.Append(_L8(" create="));
        line.AppendNum(createErr);
        line.Append(_L8("\r\n"));
        file.Write(line);
        file.Flush();
        file.Close();
        }
    fs.Close();
    }

ECOM_RECOG_EXPORT(CRecogLaunch, 0xE0DD00F4)
