/* The recogniser base: a CApaDataRecognizerType that recognises nothing, and exists only for
 * the side effect it runs when AppArc instantiates it.
 *
 * WHAT A RECOGNISER IS AND WHERE IT RUNS
 *
 * A data recogniser is a polymorphic DLL (UID2 0x10003a19) that the Application Architecture
 * server loads *into its own process* early in the boot, calls at ordinal 1 to construct, and
 * keeps in a list to classify file/buffer content by MIME type. We do not want to classify
 * anything: we want the *construction* — apparc runs it once, very early, well before the S60
 * third-party startup list. So a subclass's real work goes in OnLoad(), and every recognition
 * method is defanged to never match and never fail.
 *
 * THE FOUR DLL CONSTRAINTS (same as shim/mtm)
 *
 * 1. No writable static data. A Symbian 9.x DLL with any is refused by the loader, and
 *    tools/e32dump.py --expect-dll is the gate (dataSize==0 && bssSize==0). No file-scope
 *    mutables of any kind; anything a subclass needs to remember across calls is an instance
 *    member. A "have I already launched" flag is answered by asking the OS, not by a static.
 *
 * 2. Static constructors never run. elf2e32 sets KImageNoCallEntryPoint unconditionally, so
 *    _E32Dll is never called and no C++ static ctor in this image executes. All initialisation
 *    happens inside the exported function / the object's own constructor path.
 *
 * 3. Nothing may panic and nothing may Leave out to apparc. We run inside the AppArc server;
 *    a panic there is a panic in a system server. Every method takes the safe branch, and
 *    OnLoad() is called outside any Leave (see RECOG_EXPORT).
 *
 * 4. The shim may never be linked in. Its sources have file-scope mutables; this directory
 *    (shim/recog) is compiled instead, exactly as shim/mtm is, and USE_RECOG refuses USE_SHIM.
 *
 * WHAT A RECOGNISER WRITES
 *
 *     class CMyRecog : public CRecogBase
 *         {
 *     public:
 *         CMyRecog() : CRecogBase() {}
 *         void OnLoad();          // the side effect — non-leaving, never panics
 *         };
 *     RECOG_EXPORT(CMyRecog)      // once, at file scope, in exactly one .cpp
 */

#ifndef RECOGBASE_H
#define RECOGBASE_H

#include <e32std.h>
#include <e32base.h>
#include <apmrec.h>
#include <apmstd.h>
#include <ecom/implementationproxy.h>   /* TImplementationProxy, IMPLEMENTATION_PROXY_ENTRY */

/* A recogniser's own type UID. It is not a registered MIME implementation and never matches,
 * so the value only needs to be stable and ours; it is never looked up. Picked from the
 * developer UID range the rest of the SDK uses. */
const TUid KRecogBaseTypeUid = { 0xE0DD00F0 };

class CRecogBase : public CApaDataRecognizerType
    {
public:
    /* ENormal priority: we are not competing to classify anything, and a lower priority means
     * apparc constructs us no later than any real recogniser. Zero MIME types declared, so the
     * framework's UpdateDataTypesL loop never calls SupportedDataTypeL with a live index. */
    CRecogBase();

    /* The side effect, run once by RECOG_EXPORT after the object is fully constructed (so the
     * virtual dispatches to the subclass, which a base-constructor call could not). A subclass
     * MUST make this non-leaving and non-panicking: it runs inside the AppArc server. */
    virtual void OnLoad() = 0;

private:
    /* Recognise nothing. DoRecognizeL is where the base would set iConfidence/iDataType; we set
     * "not recognised" and return, so no buffer content is ever ascribed to us. */
    void DoRecognizeL(const TDesC& aName, const TDesC8& aBuffer);
    TUint PreferredBufSize();
    TDataType SupportedDataTypeL(TInt aIndex) const;
    };

/* The one export, at ordinal 1. AppArc hardcodes ordinal 1, and a single export is what
 * guarantees it (elf2e32 sorts export names; one name makes the sort a no-op). extern "C" so
 * the symbol is exactly what app.conf's EXPORTS names; EXPORT_C so it reaches .dynsym at all
 * (GCC gives hidden visibility by default). new (non-leaving) returns NULL on OOM, which apparc
 * tolerates; OnLoad runs only on a live object and only here, outside any Leave. */
#define RECOG_EXPORT(Class)                                            \
    extern "C" EXPORT_C CApaDataRecognizerType* CreateRecognizer()     \
        {                                                              \
        Class* self = new Class;                                       \
        if (self)                                                      \
            self->OnLoad();                                            \
        return self;                                                   \
        }

/* The ECom export, for the handset that only loads ECom recognisers (the E72: every recogniser
 * in its ROM has UID2 0x10009D8D, not the legacy 0x10003a19). Use once, at file scope, in exactly
 * one .cpp; the app.conf sets UID2=0x10009D8D and EXPORTS="ImplementationGroupProxy".
 *
 * ECom loads the plugin and calls ordinal 1 (ImplementationGroupProxy) to read the proxy table,
 * then calls the factory to instantiate the implementation whose UID the registration named. So:
 *   - the table is a file-scope `const` (no writable static data — the --expect-dll gate),
 *   - the factory uses `new (ELeave)` because ECom invokes it under a TRAP (unlike the legacy
 *     ordinal-1 path, where a Leave would escape into apparc),
 *   - OnLoad runs after a full construction, so its virtual dispatches to the subclass, and is
 *     itself non-leaving by contract (it runs inside the ECom/AppArc server).
 * `aImplUid` is the raw implementation UID value (e.g. 0xE0DD00F3); it MUST equal the
 * implementation_uid in the registration .rss, or ECom maps the request to nothing. */
#define ECOM_RECOG_EXPORT(Class, aImplUid)                                     \
    static CApaDataRecognizerType* Class##_CreateL()                           \
        {                                                                      \
        Class* self = new (ELeave) Class;                                      \
        self->OnLoad();                                                        \
        return self;                                                           \
        }                                                                      \
    static const TImplementationProxy KRecogProxyTable[] =                     \
        {                                                                      \
        IMPLEMENTATION_PROXY_ENTRY(aImplUid, Class##_CreateL)                  \
        };                                                                     \
    extern "C" EXPORT_C const TImplementationProxy* ImplementationGroupProxy(  \
        TInt& aTableCount)                                                     \
        {                                                                      \
        aTableCount = sizeof(KRecogProxyTable) / sizeof(TImplementationProxy); \
        return KRecogProxyTable;                                               \
        }

#endif /* RECOGBASE_H */
