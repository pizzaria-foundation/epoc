# Backlog: a Telegram MTM of our own

Decided: the Telegram client should register as a real message type, so it appears inside
the native Messaging application rather than only in its own window.

This is **not startable today**. Three things it needs do not exist yet, and two of them
are toolchain work with no Telegram code in them at all. This document records what the
research settled, so that whoever picks it up does not repeat it.

---

## What an MTM is, concretely

Four polymorphic DLLs, distinguished by UID2 (`sdk/epoc32/include/msvstd.hrh:37`):

| component | UID2 | base class | runs in |
|---|---|---|---|
| Server MTM | `0x10003C5E` | `CBaseServerMtm` (`mtsr.h:23`) | **the Message Server process, `msgs.exe`** |
| Client MTM | `0x10003C5F` | `CBaseMtm` (`mtclbase.h:62`) | each client application |
| UI MTM | `0x10003C60` | `CBaseMtmUi` (`mtmuibas.h`) | the Messaging application |
| UI Data MTM | `0x10003C61` | `CBaseMtmUiData` (`mtudcbas.h`) | the Messaging application |

Registration is a compiled resource of type `MTM_INFO_FILE` (`sdk/epoc32/include/mtmconfig.rh`):
an `mtm_type_uid`, a `technology_type_uid`, and one `MTM_COMPONENT_V2` per DLL above
(component uid, entry point ordinal, version, filename). Alongside it go `MTM_CAPABILITIES`
and `MTM_SECURITY_CAPABILITY_SET` — the latter declares which platform capabilities a
*client* must hold to use our MTM at all.

Registering is an active step, not a file drop: `CMtmRegistryControl::InstallMtmGroup()`
(`mtsr.h:451`). Symbian's own TextMTM example ships a separate installer application,
`txin`, for exactly this.

## The three blockers

### 1. `symbuild` cannot produce a DLL

`tools/symbuild` hardcodes `--targettype=EXE` and `--uid1=0x1000007a` in the elf2e32 call.
An MTM needs `--targettype=DLL`, `uid1=0x10000079`, ordinal 1 exported as the factory, and
frozen exports. This is new toolchain work, and it is a prerequisite for anything else here.

### 2. Writable static data is not allowed in an EKA2 DLL

Only via `EPOCALLOWDLLDATA`, and reluctantly. Rust's globals — allocator state above all —
sit exactly on that restriction. Whether our Rust can be made WSD-free, or whether
`EPOCALLOWDLLDATA` is acceptable on the E72, is an open question that should be settled by
experiment on a trivial DLL **before** any MTM code is written.

### 3. The public SDK does not ship the server-side import libraries

Checked, in `sdk/epoc32/release/armv5/lib` (1262 files):

- present: `msgs.dso`, `mtur.dso`, `smcm.dso`, `sendas2.dso`, `mmscli.dso`, `btcmtm.dso`,
  `obexclientmtm.dso`, `obexservermtm.dso`
- **absent: `mtsr.dso`, and every UI MTM library**

The headers are all there — `mtsr.h`, `mtmuibas.h`, `mtudcbas.h` — so the API can be read
but not linked against. Server MTM and UI MTM were S60-internal. That is also why no
third-party open-source MTM appears to exist to copy from.

The way out, if we take it: Symbian links imports by ordinal, and we already have
`tools/e32dump.py`. A `.dso` can be synthesised from the ordinals of the handset's own
DLLs. It is undocumented ABI, pinned to that firmware, and should be treated as the
riskiest line in this document.

## The other cost, once those are cleared

- **Capabilities.** The Server MTM loads *into* `msgs.exe`, so its DLL must carry that
  process's capabilities (`ProtServ`, `Read/WriteDeviceData`) — outside anything
  self-signed. Fine on the patched dev E72 with `keys/dev.cer`; not distributable.
- **Blast radius.** A Server MTM that panics takes `msgs.exe` with it, and the handset's
  SMS along with it. This is not a thing to debug on a phone in daily use.
- **Language.** `CBaseMtm` and friends are a C++ hierarchy with virtuals and Leave across
  their whole surface. An MTM is mostly C++ with a little Rust behind it — the inverse of
  the shim's premise, where C++ is a thin edge. Worth naming that up front rather than
  discovering it in week three.

## Where to read real MTM source

The Symbian Foundation dump on GitHub, [SymbianSource](https://github.com/SymbianSource):

| where | what |
|---|---|
| `oss.FCL.sf.mw.messagingmw` → `messagingfw/scheduledsendmtm/schedulesendmtm` | a complete, small MTM — read this one first, end to end |
| `messagingfw/msgsrvnstore/mtmbase` | the base classes themselves |
| `oss.FCL.sf.app.messaging` (`mobilemessaging`, `mmsengine`) | Nokia's UI MTMs — the part the SDK never exposed |
| `messagingfw/sendas`, `watcherfw` | SendAs, and the message-server watcher plugins |

Symbian's TextMTM example — `txtc`/`txts`/`txtu`/`txti`/`txut` plus the `txin` installer —
is the canonical walkthrough. It is **not** in our local SDK: `sdk/examples/messaging/`
carries only `biomessagemgr`, `imap4example`, `pop3example`, `sendas2example`, all
client-side.

## Suggested order, when this starts

1. Build a trivial DLL with `symbuild` and load it from `telegram.exe`. Settles blockers 1
   and 2 with no messaging code in sight.
2. Synthesise `mtsr.dso` from the E72's `mtsr.dll` and link an empty `CBaseServerMtm`
   subclass against it. Settles blocker 3.
3. Client MTM + UI Data MTM only — enough for entries to render with our icon and name.
   Reading, not sending.
4. Server MTM, on a spare handset.
5. UI MTM: viewer, then editor.

## What is worth doing before any of this

The client-side path (`CMsvSession`, `CMsvEntry`, `MMsvSessionObserver`, `RSendAs`) is
fully supported by the SDK we already have, needs only `ReadUserData+WriteUserData`, and
delivers: reading the handset's SMS, live notification when one arrives, writing entries
into the native Inbox, and sending over existing transports. A `shim_msv.cpp` in the shape
of the other shim files covers all of it. None of that work is thrown away by this task —
the Client MTM in step 3 sits on the same `CMsvSession`.
