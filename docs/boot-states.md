# What the platform is doing while it boots

A home-screen replacement on this platform is alive long before the phone is. Ours is constructed at
around 8.7 seconds of uptime — measured across eleven cold boots, 7.7 s to 9.7 s — and the system it
sits on does not become usable until somewhere between 13.7 s and past 142 s. That spread is not
noise; it is six real boots of one handset, and it is the single most important fact on this page,
because it means **no timeout is the right timeout**.

Everything here was read out of binaries and then confirmed on hardware. Where it is one and not the
other, it says so.

## The one address that matters

Publish & Subscribe category **`0x101F8766`**, key **`0x41`**. The platform's global system state.

It is not in this SDK. There is no `startupdomainpskeys.h` here, `epoc32/include` has only
`startupitem.hrh` (the start-up list resource), and — worth stating because it wasted a day — the
two UIDs in that neighbourhood are **P&S categories, not Central Repository repositories**. Looking
for `Z:\private\10202be9\101f8766.txt` is looking for a file that cannot exist.

Read it with no capability at all. Its neighbours in the same category (`0x1`, `0x2`, `0x11`, `0x31`)
answer `KErrPermissionDenied` to a process that declares nothing; this one does not.

### The shape trap

Every earlier attempt at this read came back `KErrNotFound`, and the reason was the *shape*: they
used a key UID as a category and a small integer as a key (`ps 0x101F8767 1`). That is not an absent
API, it is an address that does not exist. If you are getting NotFound here, check the shape before
concluding anything about the platform.

## The enum

Based at 100, not 0. The writer is `CStrtGlobalState::SetGlobalState` in `sysstart.exe`, which
converts an internal id 1..18 to the published value through an 18-case switch and writes it here.

| value | meaning | terminal? |
|---|---|---|
| 100 | first published state; also the "not yet set" default for every other key in this category | no |
| 101, 102 | early boot. At 102 the *startup mode* decides the branch | no |
| 103 | pre-UI phase of a normal boot | no |
| 104 | `ESwStateCriticalPhaseOK` — critical phase done, UI usable | **no** |
| 105 | `ESwStateEmergencyCallsOnly` | yes |
| 106 | test mode | yes |
| **107** | **charging** — the phone was woken by its charger | yes |
| **108** | **alarm** — woken by an alarm | yes |
| **109** | **normal, RF on** — the settled state of a phone in use | yes |
| 110 | offline / flight mode | yes |
| 111 | Bluetooth SIM Access Profile mode | yes |
| 112–115 | transitions. 114 is the one out of 107 towards normal | no |
| 116 | `ESwStateFatalStartupError` | yes |
| **117** | **shutting down** | yes |

The four names that exist as literal text anywhere in `epoc32` are `ESwStateCriticalPhaseOK`,
`ESwStateEmergencyCallsOnly` and `ESwStateFatalStartupError` (all three in `startup.exe`'s trace
strings), plus the `EStartupMode*` set below. The rest of the table is the conversion table itself.

**104 is the interesting one.** The UI is usable there — `aiidleint` is willing to bring the native
idle screen forward at 104 — but it is *not* terminal: the state machine always moves on to 109 or
110. Treating it as arrival hands over moments before the platform moves again; treating it as "still
booting" is correct and is what this SDK does.

### Key `0x42`: why the phone booted

Same category. 100 Normal / 101 Alarm / **102 Charging** / 103 Test (exact strings in
`StartupMediatorPlugin.dll`). At state 102 the state machine branches on this: Normal → 103,
Alarm → 108, **Charging → 107**, Test → 106.

So "charging mode" is not a flag beside the state. It *is* a state, and it is 107.

## The two behaviours that decide how a home screen must act

**A charging boot is a phone on its way back off.** In 107 (and in 108) `SysAp` arms a 500 ms
periodic — `CSysApAppUi::StartShutDownTimerOnAlarmAndChargingStates` — whose callback ends in
`DoShutdownL`, which requests state 117. Measured on the handset: born at 107 at 8.5 s of uptime,
117 at 19.5 s, powered off. A home screen that shows a loading bar here is promising something that
will never happen, and one that fights for the foreground is fighting the platform's charging screen
for a display nobody is looking at.

**The native idle screen grabs the foreground once, on an edge.** `aiidleint.dll` subscribes to this
exact category and key, and when the value becomes 104, 109, 110 or 111 it calls
`TApaTaskList::FindApp` and brings the native idle forward — guarded so it happens **once per boot**.

That one-shot is why the obvious approach does not work and the less obvious one does:

- Insisting on `BringToForeground` during the contested window **loses**. Measured: 17 to 27 calls
  per boot across thirteen logged sessions, every one returning `KErrNone`, the native screen on top
  regardless.
- Re-asserting on the *edge into a terminal in-use state* wins, because by then the native screen has
  spent its one shot.

A corollary that costs nothing to know: `HandleForegroundEventL(EFalse)` never arrives. In thirteen
sessions the launcher's `foreground` flag never went false, so any logic keyed on noticing the loss
never runs. Do not build on it.

## How this SDK uses it

`apps/launcher` maps the state to a phase and derives behaviour from that — one pure function, no
clocks, no charger reading, no sticky flags:

| phase | states | screen | asks for the foreground? |
|---|---|---|---|
| `Booting` | 100–104, 112–115 | boot splash | yes, every frame, no deadline |
| `InUse` | 109–111 | the home | once, on the edge in |
| `Charging` | 107 | charging screen | never |
| `Alarm` | 108 | charging screen | never |
| `Leaving` | 105, 106, 116, 117 | nothing | never |
| `Unknown` | unreadable, or outside the enum | pre-state behaviour | as before |

Flight mode moves between 109, 110 and 111 — all `InUse` — so toggling it hours after a boot is not
an edge and the home screen does not jump in front of whatever is on screen. That case is the reason
the rule is an edge and not a level.

## Reading it yourself

- `tools/probe-startup.sh` — one pass of 62 property reads over the remote shell, with positive
  controls (HWRM, `sacls.h`, a public CenRep key) and negative controls. Transcript in
  `docs/reference/startup-probe.txt`.
- `ps 0x101f8766 0x41` in the remote shell, for the single value.
- The launcher logs its own birth and every transition. `docs/reference/launcher-fixed.txt` and
  `launcher-charging.txt` are real boots, normal and charging.

What will **not** work, both measured: a probe binary registered in the platform's start-up list
(that list runs its first instruction at 69.8 s of uptime, long after everything interesting), and
polling from a host over Bluetooth (the phone's stack does not answer until ~67 s). The window that
matters is 0–20 s, and the only instrument that reaches it is code already running there.

## Where the numbers came from, and how to get more

`sdk/epoc32/release/winscw/udeb/` holds 3729 emulator binaries for this exact platform, as **x86
PE**. `objdump -d` works on them, and `file offset + 0x400000 == VMA`. That includes the whole boot
path: `sysstart.exe`, `startup.exe`, `sysap.exe`, `SPLASHSCREEN.exe`, `aiidleint.dll`, `aifw.dll`,
`StartupMediatorPlugin.dll`, `autolock.exe`.

Read these before reasoning about the handset. It is faster and more definite than the device, and
far kinder than the ROM, whose images are XIP with UTF-16 strings and no relocations.

Three traps that cost real time:

- **`sysstart.exe` contains at least five different enums based at 100** — global state, startup
  mode, startup reason, IPC opcodes and adaptation command ids. A bare run of `push 0x64 … push 0x7f`
  is most likely opcodes, not states. Find the writer of the key you care about and follow *its*
  conversion function.
- The trace `STARTER # Entering global state %d` prints the **internal id 1..18**, not the published
  value. "State 8" on the wire is 107 in the property.
- Constants usually live in `.rdata`, not as immediates. To find the code that uses one: locate the
  4-byte value in the file, map file offset to VMA through the section table, then grep the
  disassembly for `ds:0x<vma>`.
