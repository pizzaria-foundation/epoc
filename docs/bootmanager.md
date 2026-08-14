# Boot manager — order and restart policy at boot

`apps/bootctl` (an editor with a menu icon) and `apps/bootd` (a headless supervisor) ship as one
signed SIS. Together they give S60 3rd two things the platform does not have: a **boot order** and a
**restart policy**.

## Why an app and not a patch

The S60 Startup List Management API can register an executable to run at boot and nothing more.
Read the SDK's own headers:

- `sdk/epoc32/include/startupitem.hrh` — `enum TStartupExceptionPolicy { EStartupItemExPolicyNone = 0 };`
  One value. "Do nothing." The API spec says the same in prose.
- `sdk/epoc32/include/startupitem.rh:44` — `STARTUP_ITEM_INFO` is `version`, `executable_name`,
  `recovery`, and three reserved fields. **No order, phase, or priority member.**
- The `[UID].rsc` in `\private\101f875a\import\` is consumed by the Software Installer **once, at
  install time**. It is not re-read per boot, so editing it at runtime changes nothing.

So there is no platform setting to expose in a UI, and no byte in ROM to patch — a patch would only
change a path string in a struct whose policy field has one legal value. What is missing is
*behaviour*, and behaviour is userland code. No RomPatcher+ involvement anywhere in this feature.

## Shape

```
boot
 └─ S60 Starter
     └─ bootd.exe                    ← the ONLY item in the platform start-up list
         ├─ +8 s   launch [1]        order = position in the config, nothing else
         ├─ +2 s   launch [2]
         └─ watch, restart by policy (never / N times / always)
```

`apps/bootctl` never launches or stops anything; it edits `C:\Data\bootd\boot.cfg` and shows what
bootd recorded. An edit takes effect at the next boot. That is the whole contract, and it is why a
mistake costs a reboot rather than a running phone.

## Layout

| Piece | Path | What it is |
|---|---|---|
| Codec + supervisor | `crates/symbian-bootcfg` | Pure, no I/O. The config and status formats, and the whole supervisor as a state machine. 39 host tests. |
| Screens | `crates/symbian-bootctl` | Four tabs. No file access — handed a config, hands one back. 11 host tests. |
| Supervisor | `apps/bootd` | Headless glue: files, one timer, `process::is_running`. |
| Editor + package | `apps/bootctl` | The GUI app, and the SIS that carries both binaries. |

## Files on the device

```
C:\Data\bootd\boot.cfg      the boot list   (bootctl writes; bootd rewrites only to auto-disarm)
C:\Data\bootd\boot.status   the last boot   (bootd writes, bootctl reads)
C:\Data\bootd\boot.count    one byte: unsettled boots in a row
C:\Data\logs_bootd.txt      DEBUG=1 log — the only window into a headless boot process
```

`C:\Data` is outside `\sys`, `\resource` and `\private`, so **neither binary declares a
capability**. A private cage would force `AllFiles` on the editor, and a protected capability has
broken an install in this repo before (`apps/killhome/app.conf`).

Both formats carry a magic, a version, and a CRC-16. That is heavier than the flat `chunks_exact`
blobs elsewhere in the repo, on purpose: a shortcut list that decodes wrong costs a wrong icon,
this one costs a boot. A refused blob means *launch nothing*, never *guess*.

## The finding that shaped the daemon

`shim/src/shim_apparc.cpp:265` — `shim_apps_running` opens with `CCoeEnv::Static()` and returns
`SHIM_ERR_NOT_READY` when there is none. A `USE_SHIM_DAEMON` build has no `CCoeEnv`
(`shim/src/shim_daemon.cpp` is a bare `CActiveScheduler`).

> **`symbian::apps::running()` can never work inside bootd.** Death detection is
> `symbian::process::is_running` — `TFindProcess`, no window server needed.

Launching is unaffected: `shim_app_launch` uses only `RApaLsSession`, so `apps::launch(uid3)` is
fine headless. Consequence: bootd launches only **AppArc-registered apps**. Headless executables
with no `_reg.rsc` (notifd, netd) are not in the roster and are out of scope — the launcher still
starts those.

Related: `cone` and `ws32` are **not** in bootd's import table even though `shim_apparc.cpp`
references them, because `--gc-sections` drops the unreachable task-list half first. Verified with
`tools/e32dump.py` on the built image (7 DLLs, neither present) rather than assumed — an unresolved
ordinal makes an image fail to load in silence.

## Safety

- **bootd never kills anything.** It only creates. That removes the whole "the supervisor took the
  phone down" class of failure.
- **It refuses to supervise itself or bootctl**, checked in both the supervisor and the picker.
  Relaunching itself forks forever; relaunching the editor covers the user's screen every few
  seconds.
- **Per-entry budget** — `Never` = 0 restarts, `Times(n)` = n, `Always` = unbounded. An entry that
  burns a bounded budget is written back to the config as `enabled = false, auto_disarmed = true`,
  so the next boot comes up clean with no intervention. bootctl shows *why* it is off.
- **Global ceiling** (default 10 restarts per boot) stops everything at once if several flap.
- **Per-entry backoff** 5 s → 10 s → … → 5 min, so a crash-looping app is retried on its own
  schedule, not on every poll.
- **Safe mode** — `boot.count` increments at start and is cleared only after 60 quiet seconds.
  Three unsettled boots and bootd launches nothing, records `Mode::Safe`, and exits. The Boot tab's
  "Reset" softkey clears the counter; that is deliberately a person's decision.
- **Death is not declared lightly**: a 20 s launch grace, two consecutive missed observations, and
  an entry is never restarted unless it has been *seen alive at least once* since its launch. That
  last rule is what stops an app whose process UID3 differs from its app UID3 from being restarted
  forever.

**Residual risk, stated plainly:** all of this catches an app that *dies*. An app that launches
successfully and then wedges the phone reads as healthy and will be kept. Recovery there is
uninstalling the package from App. mgr.

## Polling cost

The supervise tick uses `symbian::backoff::Backoff` at 15 s → 300 s: healthy rounds double the
interval, any death snaps it back to the base rate. Once every remaining policy is `Never` and the
sequence is done, bootd exits — which also keeps the package uninstallable, since a live executable
pins `\sys\bin`.

## Building

```sh
tools/epoc build apps/bootd      # FIRST — EXTRA_EXES fails hard if the exe is missing
tools/epoc build apps/bootctl    # produces the signed bootctl.sisx with both binaries
```

The bootctl build must print `bundling bootd.exe` and
`autostart: [E0AA0010].rsc → \private\101f875a\import\`.

`SIGN=1` because the installer honours a start-up item only from a signed package. A "certificate
error" on install means the **handset clock is in the past**, not that the signature is wrong.

### The start-up resource

`apps/bootctl/data/bootctl_startup.rss` is hand-written, which `tools/symbuild:956` prefers over the
shared template. It has to be: the template substitutes `@NAME@` = the packaging app, which would
autostart the GUI editor. The item names `bootd.exe` while the import file is keyed by *bootctl's*
UID3 — the installer reads the path from inside the resource, so the two need not agree.

Verified byte-for-byte against the launcher's proven resource:

```
00000000: 6b4a 1f10 0000 0000 0000 0000 19fd 48e8  kJ............H.
00000010: 0132 0001 0002 0014 1421 3a5c 7379 735c  .2.......!:\sys\
00000020: 6269 6e5c 626f 6f74 642e 6578 6508 0000  bin\bootd.exe...
```

Same `0x101F4A6B` compiled-resource header, same field layout, differing only in the path.

## UIDs

`0xE0AA0010` bootctl, `0xE0AA0011` bootd — leaving `0004`–`000F` free for the launcher family
(`0000` launcher, `0001` killhome, `0002` notifd, `0003` netd).

## Device verification

1. `tools/epoc build apps/bootd`, then `python3 tools/e32dump.py apps/bootd/build/bootd.exe` —
   header validated, and no `ws32`/`cone` in the import list.
2. `tools/epoc build apps/bootctl`; check the two log lines above; check the phone's clock; install.
3. Open **Boot manager**. Add one harmless app (Calculator), policy `Never`, delay 5 s. Back.
4. Pull `C:\Data\bootd\boot.cfg` and `xxd` it against the host encoder's output — an end-to-end
   codec check for the price of one file copy.
5. Reboot. The app should appear ~13 s in. `C:\Data\logs_bootd.txt` shows start → parse → delay →
   `launch uid=… rc=`. **If `rc` is not 0, the first-launch delay is too short for AppArc** — raise
   it on the Setup tab. This is the most likely thing to need tuning.
6. Switch it to `Always`, reboot, kill it from the task list: it should return within ~15 s, and the
   log shows the backoff.
7. Add a deliberately bad entry; confirm three reboots reach safe mode and that the culprit comes
   back disabled in the config.
8. Uninstall from App. mgr. — proves bootd exits and does not pin `\sys\bin`.
