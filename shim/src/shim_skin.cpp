/* What colour is the phone's own theme using?
 *
 * S60 themes are user-installable, and the platform keeps their colours in a table the skin server
 * owns. `AknsUtils::GetCachedColor` reads one entry of it. That is the whole of this file.
 *
 * # Why an application would want this
 *
 * `symbian-ui` ships five hand-authored palettes, and the one called `S60` says plainly in its own
 * doc comment that its colours "were chosen to match that structure, not sampled from a device". It
 * is an interpretation of the era. An application that read the *actual* table would look like the
 * rest of the phone instead of like our idea of the phone.
 *
 * # Why one generic accessor and not a function per colour
 *
 * The table has roughly sixty entries across several item IDs. The same argument `shim_hal_get`
 * makes applies unchanged: the ID table belongs on the Rust side, where it is *data* and is covered
 * by a host test, rather than here as sixty exported functions that can each be wrong in its own
 * way. A caller passes the two halves of a `TAknsItemID` and an index; this returns a colour.
 *
 * # Why there is no TRAP
 *
 * `AknsUtils::GetCachedColor` returns `TInt` and does not Leave — `aknsutils.h:801` declares it
 * without an `L` and the whole point of the "Cached" in its name is that it answers from memory the
 * skin server already handed over. Every other function in this shim TRAPs because it calls
 * something that can Leave; this one says so instead of adding a barrier that could never fire. The
 * convention in `symbian_shim.h` allows exactly that, and asks for the comment.
 *
 * # What was checked rather than assumed
 *
 * - `aknsutils.h` is in the SDK, and line 801 declares
 *   `GetCachedColor(MAknsSkinInstance*, TRgb&, const TAknsItemID&, TInt)`.
 * - `_ZN9AknsUtils14GetCachedColorEP17MAknsSkinInstanceR4TRgbRK11TAknsItemIDi` is exported from
 *   `aknskins.dso` — read out of the library with `strings`, not inferred from the header.
 * - `aknskins.dll` loads on the E72: `docs/device-dump.txt` records `present [0]` from the DLL
 *   sweep, which is a real `RLibrary::Load` and not a file check.
 * - `shim_app.cpp` already constructs the app UI with `CAknAppUi::EAknEnableSkin`, which is what
 *   creates the per-application skin instance `SkinInstance()` returns.
 *
 * The one thing that is *not* checked is whether the import resolves on the handset, because a
 * Symbian image whose import is missing does not fail — it silently never loads. `aknskins` is not
 * in the base library set, so `USE_SKIN` exists to keep that risk inside a probe until it has been
 * proven, which is the argument `USE_AKNICON` makes in `tools/symbuild` word for word.
 *
 * # Needs a UI
 *
 * `SkinInstance()` is the *application's* skin instance, created by Avkon during app-UI
 * construction. A headless daemon has no app UI and no `CCoeEnv`, so it gets SHIM_ERR_NOT_READY
 * rather than a colour — the same shape `shim_keylock.cpp` takes for the same reason.
 */

#include "symbian_shim.h"
#include "shim_priv.h"

#include <e32base.h>
#include <coemain.h>
#include <gdi.h>
#include <fbs.h>        /* CFbsBitmap — GetScanLine, never GetPixel. See below. */
#include <AknsUtils.h>
#include <AknsItemID.h>

extern "C" {

/* One entry of the active theme's colour table.
 *
 * `aMajor`/`aMinor` are the two halves of a `TAknsItemID` (a plain two-int POD, so it is built here
 * rather than crossing the ABI); `aIndex` is the index within that table. `aOut` receives 0x00RRGGBB
 * on success and is left untouched on failure — a caller that ignores the return value gets whatever
 * it initialised rather than a plausible-looking black.
 */
int32_t shim_skin_color(int32_t aMajor, int32_t aMinor, int32_t aIndex, uint32_t* aOut)
    {
    if (!aOut)
        return SHIM_ERR_ARGUMENT;
    /* No control environment means no app UI means no skin instance. Asked before
     * `SkinInstance()` rather than after, because a null instance and a headless process are the
     * same situation and only one of them has a name a caller can act on. */
    if (!CCoeEnv::Static())
        return SHIM_ERR_NOT_READY;

    MAknsSkinInstance* skin = AknsUtils::SkinInstance();
    if (!skin)
        return SHIM_ERR_NOT_READY;

    TAknsItemID id;
    id.Set(aMajor, aMinor);

    TRgb rgb;
    const TInt err = AknsUtils::GetCachedColor(skin, rgb, id, aIndex);
    if (err != KErrNone)
        return err;

    /* Packed the way `symbian-gfx`'s `Color::hex` reads it, so the Rust side needs no shuffling and
     * there is one place that knows the byte order. Alpha is dropped: the skin table has none, and a
     * caller that invented one would be inventing it here. */
    *aOut = (static_cast<uint32_t>(rgb.Red()) << 16)
          | (static_cast<uint32_t>(rgb.Green()) << 8)
          | static_cast<uint32_t>(rgb.Blue());
    return SHIM_OK;
    }

/* Sample the theme's own background bitmap on a grid.
 *
 * # Why a bitmap at all, and why this function exists
 *
 * This paragraph used to say the colour table was all greys and the hue must therefore be in the
 * bitmaps. **That was wrong**, and it is left here as the reason this function exists rather than
 * deleted, because the correction is the more useful fact.
 *
 * The claim came from reading the first eight entries of each table, where the answer genuinely is
 * white, black and grey. Measured over the *whole* set — `docs/reference/skinprobe.txt` — 8 of the 21
 * distinct colours carry real hue, and they include every seed a palette needs: an accent
 * (`0x0099cc`, QsnOtherColors[8]), a warn (`0x751001`, QsnComponentColors[24]), a chrome blue-grey
 * (`0x4b5879`, QsnTextColors[62]) and a page tint (`0x030510`). Two of those sit past the last index
 * `AknsConstants.h` documents.
 *
 * So the palette does **not** need this function. It is kept because the question it answers — what
 * does the theme's background actually look like — is still worth being able to ask, and because the
 * answer on this handset is a finding: all four background IDs return NULL from `GetCachedBitmap`,
 * which reads a cache nothing in this process had filled.
 *
 * # Samples, not an average
 *
 * This returns up to `aCap` pixels on an evenly spaced grid rather than one averaged colour, because
 * "what is the page colour" is a *decision* — mean, median, most common, corner — and decisions belong
 * in Rust where a host test can pin them. The same argument the ID table makes one function up. What
 * cannot be in Rust is the pixel read, so that is all this does.
 *
 * # GetScanLine, never GetPixel
 *
 * `CFbsBitmap::GetPixel` (fbscli ord 131) is **not importable on this handset** — an image that asks
 * for it does not run, which `shim_apparc.cpp` records as a defect it already hit and fixed. The
 * proven path is one `GetScanLine` per row asking for `EColor64K`, which is already RGB565 and lets
 * the font-and-bitmap server do the display-mode conversion server-side. This mirrors that file
 * exactly rather than inventing a second recipe.
 *
 * # The bitmap is borrowed, not owned
 *
 * `GetCachedBitmap` hands back a pointer the skin server still owns — the name says cached. It must
 * not be deleted here, and it may be recycled after the call returns, so every pixel this needs is
 * copied out before returning.
 */
int32_t shim_skin_samples(int32_t aMajor, int32_t aMinor, uint32_t* aOut, int32_t aCap,
                          int32_t* aWidth, int32_t* aHeight)
    {
    if (!aOut || aCap <= 0 || !aWidth || !aHeight)
        return SHIM_ERR_ARGUMENT;
    if (!CCoeEnv::Static())
        return SHIM_ERR_NOT_READY;

    MAknsSkinInstance* skin = AknsUtils::SkinInstance();
    if (!skin)
        return SHIM_ERR_NOT_READY;

    TAknsItemID id;
    id.Set(aMajor, aMinor);

    /* No TRAP: GetCachedBitmap does not Leave, for the same reason GetCachedColor does not — it reads
     * a cache the server already filled. GetScanLine does not Leave either. */
    CFbsBitmap* bmp = AknsUtils::GetCachedBitmap(skin, id);
    if (!bmp)
        return SHIM_ERR_NOT_FOUND;

    const TSize size = bmp->SizeInPixels();
    *aWidth = size.iWidth;
    *aHeight = size.iHeight;
    if (size.iWidth <= 0 || size.iHeight <= 0)
        return SHIM_ERR_NOT_FOUND;

    /* One row at a time, and only the rows a sample lands on. A background can be screen-sized, so
     * reading all of it to keep sixteen pixels would be a quarter of a megabyte of copying for an
     * answer that does not improve. */
    const TInt side = 4;                       /* a 4x4 grid: 16 samples, cheap and enough */
    TInt written = 0;
    TUint8 line[640 * 2];                      /* the widest row this handset can produce */

    for (TInt gy = 0; gy < side && written < aCap; gy++)
        {
        const TInt y = (size.iHeight * (2 * gy + 1)) / (2 * side);
        const TInt w = size.iWidth < 640 ? size.iWidth : 640;
        TPtr8 row(line, sizeof(line));
        bmp->GetScanLine(row, TPoint(0, y), w, EColor64K);

        for (TInt gx = 0; gx < side && written < aCap; gx++)
            {
            const TInt x = (w * (2 * gx + 1)) / (2 * side);
            const TUint16 p = static_cast<TUint16>(line[x * 2] | (line[x * 2 + 1] << 8));
            /* RGB565 -> 0x00RRGGBB, expanded so the top bits repeat into the low ones. Otherwise a
             * full-white pixel comes back as 0xF8FCF8 and every derived colour is subtly dark. */
            const uint32_t r = ((p >> 11) & 0x1F);
            const uint32_t g = ((p >> 5) & 0x3F);
            const uint32_t b = (p & 0x1F);
            aOut[written++] = (((r << 3) | (r >> 2)) << 16)
                            | (((g << 2) | (g >> 4)) << 8)
                            | ((b << 3) | (b >> 2));
            }
        }
    return written;
    }

} /* extern "C" */
