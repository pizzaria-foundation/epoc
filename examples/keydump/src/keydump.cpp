/* Reads the handset's own QWERTY keymap out of ptiengine.dll and writes it down.
 *
 * WHY THIS EXISTS
 *
 * The target handset is a Brazilian E72 and its keyboard is ABNT2, and this SDK was
 * treating it as a US QWERTY. Three things were wrong, all user-visible: no accents at
 * all (`~` then `a` typed `a`, so nobody could write "não"), no Chr/Fn symbol layer
 * beyond twelve digits (so `+` could not be typed, in an app that asks for a phone
 * number with a country code), and no notion of a keymap in which to put the fix.
 *
 * Fixing it needs a table, and `docs/device-notes.md` is unusually direct about where
 * tables of this kind must come from:
 *
 *   "on a platform with no debugger, no console and no log, build the instrument instead
 *    of guessing."
 *
 * The keyboard section of that same page records three rounds of on-device debugging, each
 * of which blamed the wrong layer, because the layout had been reasoned about rather than
 * measured. So this is the instrument. It asks the phone.
 *
 * WHY ptiengine AND NOT THE FEP
 *
 * These are not the same thing, and the difference is the whole reason this file is
 * allowed to exist. Avkon's FEP is an *input method*: taking it means implementing
 * MCoeFepAwareTextEditor and handing the FEP authority over a caret and a text buffer the
 * Rust toolkit already owns — two components holding one buffer, which is the bug rather
 * than the wiring. `ptiengine` is the layer underneath it: a keymap database with a
 * lookup function. Asking it what a key means commits us to nothing.
 *
 * And we ask exactly once, offline. The answer is baked into a static Rust table by
 * `tools/mkkeymap.py`, so nothing that ships imports this DLL, allocates this engine, or
 * pays for either. That is why the import lives in this throwaway probe and nowhere else
 * — the rule `docs/device-notes.md` states as "if a facility might not resolve, it belongs
 * in its own binary, where failing to load costs a probe rather than the report".
 *
 * WHAT IT WRITES
 *
 * A text file, one line per key per case, plus the numeric-mode bindings. Text and not a
 * binary blob for two reasons: it has to survive a trip over Bluetooth and a human
 * reading it, and a generated Rust table whose source cannot be read by eye is a table
 * nobody can audit.
 */

/* shim_priv.h rather than symbian_shim.h: this needs ShimFsSession, the shim's own file
 * server session, so the dump lands next to everything else the app writes without
 * connecting a second session. */
#include "shim_priv.h"

#include <e32std.h>
#include <e32base.h>
/* For ELangBrazilianPortuguese (76) and ELangEnglish (1). Included by name rather than
 * relied on through e32std.h, because the whole probe turns on passing the right two
 * language ids. e32const.h and not e32lang.h: the `TLanguage` enum lives in the former on
 * this SDK, and the latter does not exist here at all. */
#include <e32const.h>
#include <f32file.h>
#include <PtiEngine.h>
#include <PtiDefs.h>
#include <PtiKeyMappings.h>

extern "C" {

/* Slots the Rust side reads, so the screen can report what happened without knowing
 * anything about Symbian. -1 is "not reached", which is a different fact from 0. */
enum TSlot
    {
    ESlotErr = 0,        /* the leave code, or SHIM_OK */
    ESlotKeysBrPt,       /* keys that produced at least one character, pt-BR */
    ESlotDeadBrPt,       /* dead-key mappings found, pt-BR */
    ESlotKeysEn,         /* the same for English, as a baseline to diff against */
    ESlotNumeric,        /* numeric-mode bindings reported for pt-BR */
    ESlotBytes,          /* bytes written */
    ESlotCount
    };

int32_t keydump_run(const uint16_t* path, int32_t path_len, int32_t* out, int32_t cap);

} /* extern "C" */

namespace {

TInt gResult[ESlotCount];

/* Every key on a 4x12 QWERTY that PtiDefs.h gives a name to.
 *
 * Deliberately the full enumeration rather than a numeric range: TPtiKey mixes ASCII
 * values (EPtiKeyQwertyA = 0x41) with EStdKey values (EPtiKeyQwertyComma =
 * EStdKeyComma), so the qwerty keys are not contiguous and a for-loop over integers
 * would query nonsense and miss the punctuation — which on an ABNT2 keyboard is exactly
 * where the accents live.
 *
 * The comments are the key coordinates PtiDefs.h records, kept because a dump that says
 * which *position* a character came from is what lets the scan codes from keyprobe be
 * lined up against it. */
const TPtiKey KQwertyKeys[] =
    {
    EPtiKeyQwerty1, EPtiKeyQwerty2, EPtiKeyQwerty3, EPtiKeyQwerty4, EPtiKeyQwerty5,
    EPtiKeyQwerty6, EPtiKeyQwerty7, EPtiKeyQwerty8, EPtiKeyQwerty9, EPtiKeyQwerty0,
    EPtiKeyQwertyA, EPtiKeyQwertyB, EPtiKeyQwertyC, EPtiKeyQwertyD, EPtiKeyQwertyE,
    EPtiKeyQwertyF, EPtiKeyQwertyG, EPtiKeyQwertyH, EPtiKeyQwertyI, EPtiKeyQwertyJ,
    EPtiKeyQwertyK, EPtiKeyQwertyL, EPtiKeyQwertyM, EPtiKeyQwertyN, EPtiKeyQwertyO,
    EPtiKeyQwertyP, EPtiKeyQwertyQ, EPtiKeyQwertyR, EPtiKeyQwertyS, EPtiKeyQwertyT,
    EPtiKeyQwertyU, EPtiKeyQwertyV, EPtiKeyQwertyW, EPtiKeyQwertyX, EPtiKeyQwertyY,
    EPtiKeyQwertyZ,
    /* The punctuation keys, and the reason enumerating *named* keys is enough.
     *
     * TPtiKey has no EPtiKeyQwertyCedilla and no accent keys, which looks at first like
     * this approach cannot reach them. It can, and the header says why: the enum names
     * *positions*, not characters — "non-shifted EPtiKeyQwertyHash produces
     * '#'-character if input language is English, but will produce '+'-character if
     * input language is Danish". An ABNT2 keyboard is the same 4x12 grid with different
     * characters printed on it, so Ç and the dead keys arrive as whatever these
     * punctuation positions map to under pt-BR. That difference is exactly what the
     * English baseline dump makes visible. */
    EPtiKeyQwertyPlus, EPtiKeyQwertyMinus, EPtiKeyQwertyComma, EPtiKeyQwertySemicolon,
    EPtiKeyQwertyFullstop, EPtiKeyQwertyHash, EPtiKeyQwertySlash, EPtiKeyQwertyApostrophe,
    EPtiKeyQwertySpace,
    };

const TInt KQwertyKeyCount = sizeof(KQwertyKeys) / sizeof(KQwertyKeys[0]);

const TPtiTextCase KCases[] =
    { EPtiCaseLower, EPtiCaseUpper, EPtiCaseChrLower, EPtiCaseChrUpper };

const TInt KCaseCount = 4;

/* The four cases, named the way the generated Rust table names its columns, so a human
 * reading the dump and a human reading layout_abnt2.rs are reading the same words. */
const char* const KCaseNames[] = { "lower", "upper", "chr-lower", "chr-upper" };

/* MappingDataForKey wants a TDes. 64 is generous: the longest real mapping is a Chinese
 * "get all" list, which we never ask for, and a Latin key carries at most a character and
 * a dead-key marker. */
const TInt KMaxMapping = 64;

/* ------------------------------------------------------------------- the writer -- */

/* Append a NUL-terminated ASCII string. Kept 8-bit throughout: the file is a dump for a
 * python script and a human, so UTF-16 would double its size and buy nothing, and the
 * characters that are *not* ASCII are written as hex rather than as themselves — a `ã` in
 * a file whose encoding nobody declared is how you lose a measurement. */
void Put(RFile& aFile, const char* aStr)
    {
    TPtrC8 p(reinterpret_cast<const TUint8*>(aStr));
    aFile.Write(p);
    gResult[ESlotBytes] += p.Length();
    }

void PutInt(RFile& aFile, TInt aValue)
    {
    TBuf8<16> buf;
    buf.Num(aValue);
    aFile.Write(buf);
    gResult[ESlotBytes] += buf.Length();
    }

/* Four uppercase hex digits, which is how every code unit in this file is written. */
void PutHex4(RFile& aFile, TUint aValue)
    {
    TBuf8<8> buf;
    buf.NumFixedWidth(aValue & 0xFFFF, EHex, 4);
    buf.UpperCase();
    aFile.Write(buf);
    gResult[ESlotBytes] += buf.Length();
    }

/* ---------------------------------------------------------------- one language -- */

/* A dead key, as the keymap data marks one.
 *
 * The first run of this probe looked for KPtiKeyDataDeadKeySeparator (0xFFFF) and found
 * none, on a keyboard that certainly has dead keys. That constant is real but it is not
 * this: it separates sections of the *dead-key table* blob, a different structure
 * (`CPtiQwertyKeyMappings::iDeadKeyData`). What appears in a key's own mapping is a code in
 * the range 0xF000..0xF005, and the platform's own test for it is
 * `CPtiQwertyKeyMappings::IsDeadKeyCode` — inline in PtiKeyMappings.h, and copied here
 * exactly rather than paraphrased:
 *
 *     (aChar & 0xff00) == 0xf000 && (aChar & 0xff) <= 5
 *
 * So there are at most six dead keys per layout, indexed 0..5 by the low byte. ABNT2 needs
 * five (´ ` ^ ~ ¨), which fits.
 *
 * Note what this does *not* give: the mark itself. 0xF001 says "dead key number one", not
 * "acute" — the character lives in the dead-key table, which no public API exposes. That is
 * what the composition probe below is for: ask the engine to type the key and then a vowel,
 * and it says what the pair produces. Which is a better answer anyway, since it measures the
 * composition rather than inferring it from a mark. */
TBool IsDeadKeyCode(TUint16 aChar)
    {
    return ((aChar & 0xff00) == 0xf000) && ((aChar & 0xff) <= 5);
    }

/* Dump every key in every case for one language.
 *
 * The line format, one per key and case that produced anything:
 *
 *     K <ptikey-hex> <case-name> <n> <unit> <unit> ...
 *
 * The units are raw UCS-2, in hex, exactly as the engine returned them, with nothing
 * filtered out — dead-key codes and any 0xFFFF included. A dump that decided what to keep
 * would be a dump that cannot answer a question we did not think to ask, which is how the
 * first run of this probe came back empty-handed.
 */
void DumpLanguageL(RFile& aFile, CPtiEngine& aEngine, TInt aLanguage,
                   const char* aTag, TInt& aKeysOut, TInt& aDeadOut)
    {
    aKeysOut = 0;
    aDeadOut = 0;

    Put(aFile, "\n# language ");
    Put(aFile, aTag);
    Put(aFile, " id ");
    PutInt(aFile, aLanguage);
    Put(aFile, "\n");

    /* ActivateLanguageL returns an error rather than leaving when the language or the
     * mode is not available, and that return value is itself a finding: a handset with no
     * pt-BR qwerty keymap is the case where this whole approach does not apply, and it
     * must be recorded rather than mistaken for an empty keyboard. */
    const TInt err = aEngine.ActivateLanguageL(aLanguage, EPtiEngineQwerty);
    Put(aFile, "# activate rc ");
    PutInt(aFile, err);
    Put(aFile, "\n");
    if (err != KErrNone)
        return;

    for (TInt k = 0; k < KQwertyKeyCount; k++)
        {
        for (TInt c = 0; c < KCaseCount; c++)
            {
            TBuf<KMaxMapping> map;
            aEngine.MappingDataForKey(KQwertyKeys[k], map, KCases[c]);
            if (map.Length() == 0)
                continue;

            Put(aFile, "K ");
            PutHex4(aFile, static_cast<TUint>(KQwertyKeys[k]));
            Put(aFile, " ");
            Put(aFile, KCaseNames[c]);
            Put(aFile, " ");
            PutInt(aFile, map.Length());
            for (TInt i = 0; i < map.Length(); i++)
                {
                Put(aFile, " ");
                PutHex4(aFile, map[i]);
                if (IsDeadKeyCode(map[i]))
                    aDeadOut++;
                }
            Put(aFile, "\n");
            aKeysOut++;
            }
        }
    }

/* The Chr-layer bindings for "0123456789pw+#*", straight from the engine.
 *
 * This is the direct answer to the `+` that could not be typed: it says which key and
 * which case produce it on *this* language's layout, which is exactly the pair the
 * generated table is keyed by. Worth asking for separately even though the per-key dump
 * above covers the same ground, because it is the platform's own answer to "where are the
 * digits on this keyboard" and disagreement between the two is worth seeing.
 */
void DumpNumericL(RFile& aFile, CPtiEngine& aEngine, TInt aLanguage, TInt& aCountOut)
    {
    aCountOut = 0;
    RArray<TPtiNumericKeyBinding> bindings;
    CleanupClosePushL(bindings);
    aEngine.GetNumericModeKeysForQwertyL(aLanguage, bindings);
    Put(aFile, "# numeric bindings ");
    PutInt(aFile, bindings.Count());
    Put(aFile, "\n");
    for (TInt i = 0; i < bindings.Count(); i++)
        {
        /* N <char-hex> <ptikey-hex> <case-index> */
        Put(aFile, "N ");
        PutHex4(aFile, bindings[i].iChar);
        Put(aFile, " ");
        PutHex4(aFile, static_cast<TUint>(bindings[i].iKey));
        Put(aFile, " ");
        PutInt(aFile, static_cast<TInt>(bindings[i].iCase));
        Put(aFile, "\n");
        aCountOut++;
        }
    CleanupStack::PopAndDestroy(&bindings);
    }

void RunL(const TDesC& aPath)
    {
    RFs* fs = NULL;
    User::LeaveIfError(ShimFsSession(fs));

    /* C:\Data\ exists on a stock S60 handset, but "exists on the ones we have seen" is not
     * a guarantee, and Replace does not create a missing directory — it returns
     * KErrPathNotFound, which would surface as an error code on the probe's screen with no
     * hint that the path was the problem. KErrAlreadyExists is the normal answer here and
     * is not a failure. */
    const TInt mk = fs->MkDirAll(aPath);
    if (mk != KErrNone && mk != KErrAlreadyExists)
        User::Leave(mk);

    RFile file;
    /* Replace, not append: a second run must produce a whole dump rather than a file
     * with two of them, because the generator has no way to tell where one ended. */
    User::LeaveIfError(file.Replace(*fs, aPath, EFileWrite | EFileStream));
    CleanupClosePushL(file);

    Put(file, "# keydump 1 - the handset's own qwerty keymap, via CPtiEngine\n");
    Put(file, "# K <ptikey> <case> <n> <ucs2>... ; 0xFFFF is the dead-key separator\n");
    Put(file, "# N <char> <ptikey> <case-index>  ; numeric-mode bindings\n");

    CPtiEngine* engine = CPtiEngine::NewL();
    CleanupStack::PushL(engine);

    /* pt-BR first: it is the answer we want, and if the engine dies partway through we
     * would rather have it than the baseline. */
    DumpLanguageL(file, *engine, ELangBrazilianPortuguese, "brazilian-portuguese",
                  gResult[ESlotKeysBrPt], gResult[ESlotDeadBrPt]);
    DumpNumericL(file, *engine, ELangBrazilianPortuguese, gResult[ESlotNumeric]);

    /* English as a control. The *difference* between the two dumps is precisely what
     * "American instead of ABNT" means, so having both turns a claim into a diff. It also
     * catches a whole class of failure: if the two come back identical, the engine is not
     * switching layouts and the pt-BR dump means nothing. */
    TInt deadEn = 0;
    DumpLanguageL(file, *engine, ELangEnglish, "english", gResult[ESlotKeysEn], deadEn);

    Put(file, "# end\n");

    CleanupStack::PopAndDestroy(engine);
    CleanupStack::PopAndDestroy(&file);
    }

} /* namespace */

extern "C" {

/* Run the whole dump into `path`, filling `out` with the slots above.
 *
 * One call, synchronously: PtiEngine's lookups are plain function calls into a database,
 * not asynchronous requests, so there is no active object and nothing to poll. That is
 * the opposite of imgprobe, and the reason this probe is thirty lines of driving rather
 * than a state machine.
 */
int32_t keydump_run(const uint16_t* path, int32_t path_len, int32_t* out, int32_t cap)
    {
    for (TInt i = 0; i < ESlotCount; i++)
        gResult[i] = -1;
    gResult[ESlotBytes] = 0;

    TPtrC p(path, path_len);
    TRAPD(err, RunL(p));
    gResult[ESlotErr] = err;

    if (out)
        {
        const TInt n = (cap < ESlotCount) ? cap : ESlotCount;
        for (TInt i = 0; i < n; i++)
            out[i] = gResult[i];
        }
    return err;
    }

} /* extern "C" */
