# opus

The vendored libopus, decode only, behind a safe API.

Every `unsafe` in the audio path lives here. The same arrangement as `symbian-sys`
under `symbian`: a crate whose job is to hold the FFI so the crates above it can be
`forbid(unsafe_code)` and mean it. What leaves here is `Decoder::decode`.

## Why a C library and not a decoder written here

Opus is two codecs - SILK for speech, CELT for music - plus a hybrid mode that runs
both and crosses over. A conforming decoder is tens of thousands of lines, and the
specification is normative *by reference to the reference implementation*. Writing
one to decode voice messages on a phone would be the project instead of a part of it.

libopus is also small, has no allocator of its own worth fighting, and builds for
`armv5te` without ceremony. `build.rs` compiles it with the SDK's cross compiler.

## Decode only

The encoder is not built. Nothing in this SDK records audio, and leaving it out is
roughly half the object size on a device where the whole application has to fit in a
demand-paged image.

## Using it

Do not, directly. [`symbian-audio`](../symbian-audio) owns the path from a container
to something the platform will play, and that is the layer an application wants:

    ogg -> opus packets -> [this crate] -> PCM i16 -> wav -> MMF plays it

## Vendoring

The sources are under `vendor/`, which is gitignored: it is third-party code with its
own licence and its own release cadence, and this repository does not redistribute it.
`docs/getting-started.md` says where to get it.

---

Part of [epoc](../../README.md), a Rust SDK for Symbian S60 3rd Edition. MIT licensed; see
`LICENSE` at the repository root. Written with AI assistance,
and every hardware claim in it was measured rather than reasoned about.
