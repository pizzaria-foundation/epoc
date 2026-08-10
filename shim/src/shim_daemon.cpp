/* The headless entry: E32Main for a resident daemon with no window and no Avkon.
 *
 * This file replaces shim_app.cpp in a daemon build (USE_SHIM_DAEMON=1). Everything the
 * daemon needs — timers, sockets, the filesystem/app/key monitors, Publish & Subscribe —
 * is an ordinary active object that runs under any CActiveScheduler, so the whole GUI stack
 * (Avkon application/document/appui, the control, the framebuffer, the CIdle pump) is gone
 * and with it the reasons a background app gets closed: there is no window group to appear
 * in the task list, and no CEikAppUi to receive the "close background applications"
 * shutdown broadcast. The process simply runs until it is told to stop.
 *
 * # Why UID2 is not KUidApp
 *
 * An E32 image whose UID2 is KUidApp (0x100039ce) is an application: AppArc registers it,
 * the shell lists it, and the window server tracks it. This image is built with UID2=0, so
 * it is a plain executable — launched by the controller with RProcess::Create, never by the
 * shell. That, plus the absence of a window group, is what makes it invisible to everything
 * that would otherwise close it.
 *
 * # The pump
 *
 * The GUI build drains the event ring from a CIdle that re-arms forever, which is free only
 * because the window server blocks the thread between frames. A headless daemon has nothing
 * to block it, so a self-rearming CIdle would spin rust_step at 100% CPU. Instead a
 * CPeriodic wakes the drain a few times a second — latency the ring (64 slots) easily
 * absorbs for the sparse events a monitor produces, at a wake rate a phone can afford. The
 * daemon's own 1.5 s poll timer (Rust side, via shim_timer) is unaffected; this only
 * governs how promptly a completed socket or fs-watch reaches Rust.
 */

#include "shim_priv.h"

#include <e32base.h>
#include <e32std.h>

/* Provided by the daemon_entry! macro, exactly as for the GUI build. */
extern "C" void rust_app_start(void);
extern "C" void rust_app_stop(void);
extern "C" void rust_step(void);

namespace {

/* How often the drain wakes. 200 ms: four to five drains a second, well inside the ring's
 * capacity for anything short of a pathological event storm, and cheap enough that a
 * resident daemon does not measurably move the battery. */
const TInt KPumpIntervalUs = 200 * 1000;

TBool gExitRequested = EFalse;
CPeriodic* gPump = NULL;

TInt PumpTick(TAny*)
    {
    rust_step();
    if (gExitRequested)
        {
        /* Stop the scheduler; MainL resumes after Start() returns and runs teardown. The
         * periodic cancels itself so no further tick fires during teardown. */
        if (gPump)
            gPump->Cancel();
        CActiveScheduler::Stop();
        return EFalse;
        }
    return ETrue;
    }

void MainL()
    {
    /* Bring the app up first: rust_app_start constructs the Daemon, which attaches to the
     * network route and arms its timers through the shim's active objects. */
    rust_app_start();

    gPump = CPeriodic::NewL(CActive::EPriorityIdle);
    gPump->Start(KPumpIntervalUs, KPumpIntervalUs, TCallBack(&PumpTick, NULL));

    /* Signal the launcher that the daemon is up. The controller's shim_process_start blocks
     * on this rendezvous, so it reports success only once the scheduler and the pump exist,
     * not merely because a process object was created. */
    RProcess::Rendezvous(KErrNone);

    CActiveScheduler::Start();

    /* Reached only after PumpTick stopped the scheduler. Tear down in the same order the
     * GUI build does: tell Rust first (it may hold pointers into shim state), then close
     * every facility that owns a kernel handle, so nothing leaks past process exit. */
    rust_app_stop();
    ShimTimersCleanup();
#ifdef SHIM_USE_PROP
    ShimPropCleanup();
#endif
    ShimFilesCleanup();
#ifdef SHIM_USE_NET
    ShimNetCleanup();
    ShimWorkCleanup();
#endif

    delete gPump;
    gPump = NULL;
    }

} /* namespace */

/* The stop signal. The Rust side calls shim_request_exit() from rust_step once its
 * DaemonApp reports should_exit — which happens when the P&S stop property arrives. Same
 * contract as the GUI build; the two definitions never coexist because exactly one of
 * shim_app.cpp / shim_daemon.cpp is compiled. */
void ShimRequestExit()
    {
    gExitRequested = ETrue;
    }

extern "C" void shim_request_exit(void)
    {
    ShimRequestExit();
    }

GLDEF_C TInt E32Main()
    {
    CTrapCleanup* cleanup = CTrapCleanup::New();
    if (!cleanup)
        return KErrNoMemory;

    CActiveScheduler* scheduler = new CActiveScheduler;
    if (!scheduler)
        {
        delete cleanup;
        return KErrNoMemory;
        }
    CActiveScheduler::Install(scheduler);

    TRAPD(err, MainL());

    delete scheduler;
    delete cleanup;
    return err;
    }
