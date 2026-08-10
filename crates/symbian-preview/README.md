# symbian-preview

Any screen to a PNG, on the host. 3 tests.

The unit tests prove what a machine can check: containment, symmetry, clamping. They
cannot tell you whether a 9-pixel bell reads as a bell, or whether a bevel is visible at
all. That is what a contact sheet is for, and this crate is the machinery behind one.

Host-only. It uses `std`, writes files, and never reaches a device build - the same
standing as `symbian-sim`, and a crate applications pull in under `[dev-dependencies]`.

## What it gives you

    Sheet              a screen-sized RGB565 buffer, a Canvas over it, save(dir, name)
    Atlases::load      the font atlases, chained the way the device chains them
    with_fonts         builds the fonts and hands you a symbian_ui::Fonts
    with_themes        the same, with dark and light Themes over them
    blit_zoom          magnify a region with a gutter grid, for judging small icons
    sdk_root           finds crates/symbian-ui/assets by walking up, or reads EPOC_SDK

## What it deliberately does not contain

Any particular screen. The SDK's own sheets - the rasterizer smoke test, the icon set,
the surface primitives - live in `tools/preview`. An application's sheets live with the
application, so its scenes travel with the code they document:

    // apps/myapp/examples/preview.rs
    let atlases = Atlases::load(&symbian_preview::sdk_root());
    atlases.with_themes(|dark, _light| {
        let mut sheet = Sheet::new(E72_SCREEN);
        my_screen.draw(&mut sheet.canvas(), dark);
        sheet.save("preview-out", "10-my-screen");
    });

That split is why `sdk_root` walks the filesystem instead of using
`CARGO_MANIFEST_DIR`: a preview run from an application's own repository still has to
find the atlases in whatever checkout of the SDK is around.

## Two decisions worth knowing

**The fonts arrive through a closure.** `Atlases` owns the atlas bytes; the
`BitmapFont`s that borrow them are built inside `with_fonts` and never stored. That
keeps it a plain struct instead of a self-referential one, and the borrow checker never
enters the conversation.

**Magnification draws the grid first, then the pixels on top.** Each source pixel becomes
a `zoom-1` block with a one-pixel gutter. Drawing the grid *over* the blocks - the obvious
way - clips a pixel off every one of them and turns three solid rules into what looks like
five stripes. A contact sheet that lies is worse than no contact sheet.

## The PNG writer

`png`, ~130 lines, no dependencies. The zlib stream is built from *stored* deflate
blocks, which is legal and trivially correct. A 320x240 screenshot comes out around
230 KB that way, which is irrelevant for something a developer looks at once and saves
pulling in a deflate implementation the SDK would otherwise never use.

Output is magnified 2x, nearest-neighbour: a 320x240 PNG at 1:1 on a modern display is
about the size of a postage stamp, and a pixel stays a pixel.

---

Part of [epoc](../../README.md), a Rust SDK for Symbian S60 3rd Edition. MIT licensed; see
`LICENSE` at the repository root. `symbian` in this crate's name is descriptive, not a claim
on somebody else's trademark - the repository README says more. Written with AI assistance,
and every hardware claim in it was measured rather than reasoned about.
