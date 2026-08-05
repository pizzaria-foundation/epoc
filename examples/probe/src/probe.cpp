/* The smallest thing that can prove the loader accepts our E32 images.
 *
 * No Avkon, no window, no graphics: just E32Main writing a file and returning.
 * If C:\Data\<name>.txt appears, the image loaded and our code ran, and every
 * remaining problem is in the GUI layer. If it does not, the E32 format itself is
 * wrong and nothing above it matters.
 *
 * Two imports only (euser for the descriptors, efsrv for the file), so the PLT is
 * tiny -- which also tests whether the 72 R_ARM_JUMP_SLOT entries the Avkon build
 * generated were the problem.
 */
#include <e32base.h>
#include <f32file.h>

GLDEF_C TInt E32Main()
    {
    RFs fs;
    if (fs.Connect() != KErrNone)
        return KErrGeneral;
    _LIT(KDir, "C:\\Data\\");
    fs.MkDirAll(KDir);

    RFile f;
    TInt err = f.Replace(fs, KOutPath, EFileWrite | EFileShareAny);
    if (err == KErrNone)
        {
        f.Write(_L8("E32Main ran\r\n"));
        f.Flush();
        f.Close();
        }
    fs.Close();
    return KErrNone;
    }
