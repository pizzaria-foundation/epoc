# symbian-audio

Ogg/Opus in, a playable WAV out. `no_std`, `forbid(unsafe_code)`, 28 tests.

The handset knows AMR, AAC and MP3 — its FourCC list (`mmf/common/mmffourcc.h`) stops
there. Opus is from 2012 and the phone is from 2008, so an Opus voice message has to be
taken apart and rebuilt into something the platform will accept:

```text
bytes ──[ogg]──▶ Opus packets ──[opus]──▶ PCM i16 ──[wav]──▶ RIFF/WAVE ──▶ MMF plays it
```

Written for the reference client's voice notes and lifted out of it unchanged, because
none of it is about any one application.

## The 44 bytes that buy the easy API

The interesting decision is in `wav`, and it is not "write a WAV because that is normal".

MMF picks its format plugin by matching the *header* against a detection string, and the
shipped WAV plugin registers `RIFF????WAVE` — four wildcard bytes where the size goes.
Raw PCM is the one standard format the resolver explicitly cannot identify; the platform
guide says so. Hand it bare samples and you are forced onto `OpenUrlL`, where the format
has to be described by hand.

So 44 bytes of header are the difference between `OpenFileL` and an afternoon. The
samples must be **signed little-endian 16-bit**: the supported-codec table lists WAV as
carrying signed 16-bit and *unsigned* 8-bit, which is the RIFF convention and the
opposite of what you would assume for the 8-bit case.

## Modules

| | |
|---|---|
| `ogg` | Ogg pages to Opus packets, per RFC 7845. Lacing, page boundaries, `OpusHead` |
| `opus` | the packets to PCM, over [`opus`](../opus) — which holds the FFI so this crate does not |
| `wav` | PCM to a RIFF/WAVE file MMF will open |

## Testing

`cargo test -p symbian-audio` — 28 tests, all on the host, including a decode of a real
voice message in `src/testdata/voice.opus` (generated locally with ffmpeg; it is not
anybody's message). A container parser is all edge cases, and the ones that bite are
truncation and a page boundary landing mid-packet, so those are tested directly rather
than inferred from a successful decode.

## What is not here

Encoding, and playback. Encoding because nothing in this SDK records audio yet; playback
because it is the platform's job — hand the bytes to `symbian::fs` and the file to
`CMdaAudioPlayerUtility` through the shim's `USE_AUDIO`.

---

Part of [epoc](../../README.md), a Rust SDK for Symbian S60 3rd Edition. MIT licensed; see
`LICENSE` at the repository root. `symbian` in this crate's name is descriptive, not a claim
on somebody else's trademark - the repository README says more. Written with AI assistance,
and every hardware claim in it was measured rather than reasoned about.
