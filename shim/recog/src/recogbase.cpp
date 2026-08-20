/* CRecogBase — the recognise-nothing base. See shim/recog/inc/recogbase.h for why it exists.
 *
 * Every method here is deliberately inert. The value of this class is entirely in OnLoad(),
 * which the subclass supplies and RECOG_EXPORT calls; the recognition surface is present only
 * because CApaDataRecognizerType demands it, and it is wired so no file is ever ascribed to us
 * and nothing leaves out into the AppArc server.
 */

#include "recogbase.h"

/* ENormal (0) priority, zero declared MIME types. The base constructor stores the UID and
 * priority; iCountDataTypes starts at 0 and we leave it there, so UpdateDataTypesL has nothing
 * to enumerate and SupportedDataTypeL is never reached with a live index. */
CRecogBase::CRecogBase()
    : CApaDataRecognizerType(KRecogBaseTypeUid, CApaDataRecognizerType::ENormal)
    {
    iCountDataTypes = 0;
    }

/* Set "not recognised" and return. DoRecognizeL is the framework's one entry into a recogniser;
 * ENotRecognized (KMinTInt) is the lowest confidence, so nothing we see is ever claimed. It is
 * declared to Leave (framework signature) but this body never does. */
void CRecogBase::DoRecognizeL(const TDesC& /*aName*/, const TDesC8& /*aBuffer*/)
    {
    iConfidence = ENotRecognized;
    }

/* The framework asks how much of a file to read before calling us. We classify nothing, so ask
 * for as little as the framework will accept. */
TUint CRecogBase::PreferredBufSize()
    {
    return 0;
    }

/* Pure in the base, so it must exist. With iCountDataTypes == 0 the framework never calls this
 * with a valid index; an empty TDataType is the safe answer if it ever does. */
TDataType CRecogBase::SupportedDataTypeL(TInt /*aIndex*/) const
    {
    return TDataType();
    }
