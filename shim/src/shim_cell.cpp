/* Which cell tower this phone is talking to.
 *
 * `CTelephony::GetCurrentNetworkInfo` fills a TNetworkInfoV1 with the country code (MCC), the
 * network identity (MNC), the location area code and the cell id — which together are the key a
 * public tower database is looked up by. That is how a phone with no view of the sky still knows
 * which city it is in, and it is the only route to a position on this handset: the GPS probe
 * measured the platform's own network positioning module answering KErrGeneral, and both satellite
 * modules timing out indoors.
 *
 * ITS OWN FILE AND ITS OWN FLAG, NOT PART OF USE_LBS
 *
 * The obvious place was shim_lbs.cpp, which already owns "where am I". It is the wrong place for
 * two reasons. This adds etel3rdparty.dso, an import the GPS probe has no use for — and an import
 * that does not resolve makes an application vanish with no panic and no log, so folding it into
 * USE_LBS would put that risk into a binary that was already working. And it is not the location
 * framework at all: it is telephony, and a caller may well want one without the other.
 *
 * NOT shim_tele.cpp EITHER, WHICH IS THE CAUTIONARY TALE
 *
 * That file reads signal strength with User::WaitForRequest, and its own header documents at
 * length why that is broken: on a thread with a running CActiveScheduler, waiting consumes
 * whatever completes next — possibly a completion belonging to one of that thread's own active
 * objects — and the scheduler then dies with a stray-signal panic. Measured: nine netd sessions in
 * one log that wrote their first line and nothing else. apps/netd does not call it.
 *
 * So this is a CActive whose RunL pushes SHIM_EV_CELL onto the ring and returns. Nothing blocks,
 * and the same file that documents the mistake is next door as a reminder.
 *
 * MCC AND MNC ARE TEXT, AND THAT IS NOT A DETAIL
 *
 * TNetworkInfoV1 stores them as TBuf<4> and TBuf<8> of decimal digits, because an MNC of "06" is
 * not the same operator as one of "6" in every country. They are parsed to integers here because
 * that is what a geolocation query wants — and the parse refuses anything that is not digits
 * rather than returning a partial number, which would silently look up somebody else's tower.
 */

#include "shim_priv.h"

#ifdef SHIM_USE_CELL

#include <e32base.h>
#include <e32std.h>
#include <etel3rdparty.h>

namespace {

/* Digits to an integer, or KErrArgument. `TLex` would do this and would also accept a leading
 * sign and surrounding space; a network identity with either is not a network identity. */
TInt ParseDigits(const TDesC& aText, TInt& aOut)
    {
    if (aText.Length() == 0 || aText.Length() > 9)
        return KErrArgument;
    TInt value = 0;
    for (TInt i = 0; i < aText.Length(); i++)
        {
        const TInt c = aText[i];
        if (c < '0' || c > '9')
            return KErrArgument;
        value = value * 10 + (c - '0');
        }
    aOut = value;
    return KErrNone;
    }

class CShimCell : public CActive
    {
public:
    static CShimCell* NewL();
    ~CShimCell();

    void Read();
    TInt Get(int32_t* aMcc, int32_t* aMnc, int32_t* aLac, int32_t* aCid,
             int32_t* aAreaKnown) const;

private:
    CShimCell();
    void ConstructL();
    void RunL();
    void DoCancel();

private:
    CTelephony* iTel;
    /* The info and its package are members, and the package wraps the member. The framework writes
     * into that descriptor asynchronously, so a package built on the stack of the function that
     * issued the request is memory the request fills in after the frame is gone — the same rule
     * that shim_image.cpp's iSource comment was written for, and the same silence when it is
     * broken: a request reading a garbage length does not fail loudly, it waits. */
    CTelephony::TNetworkInfoV1 iInfo;
    CTelephony::TNetworkInfoV1Pckg iPckg;

    TBool iHaveResult;
    TInt iLastStatus;
    };

CShimCell* gCell = NULL;

CShimCell::CShimCell()
    : CActive(EPriorityStandard),
      iPckg(iInfo),
      iHaveResult(EFalse),
      iLastStatus(KErrNotReady)
    {
    CActiveScheduler::Add(this);
    }

CShimCell::~CShimCell()
    {
    Cancel();
    delete iTel;
    }

CShimCell* CShimCell::NewL()
    {
    CShimCell* self = new (ELeave) CShimCell();
    CleanupStack::PushL(self);
    self->ConstructL();
    CleanupStack::Pop(self);
    return self;
    }

void CShimCell::ConstructL()
    {
    iTel = CTelephony::NewL();
    }

void CShimCell::Read()
    {
    if (IsActive())
        return;
    iTel->GetCurrentNetworkInfo(iStatus, iPckg);
    SetActive();
    }

void CShimCell::RunL()
    {
    iLastStatus = iStatus.Int();
    iHaveResult = ETrue;

    ShimEvent e;
    e.kind = SHIM_EV_CELL;
    e.handle = 0;
    e.status = iLastStatus;
    /* iAreaKnown in `a`, because it is the field that says whether the other two mean anything —
     * a caller that read a location area code without checking it would query a database for a
     * tower the phone never named. */
    e.a = (iLastStatus == KErrNone && iInfo.iAreaKnown) ? 1 : 0;
    e.b = 0;
    e.c = 0;
    e.d = 0;
    e.native = 0;
    ShimPushEvent(e);
    }

void CShimCell::DoCancel()
    {
    if (iTel)
        iTel->CancelAsync(CTelephony::EGetCurrentNetworkInfoCancel);
    }

TInt CShimCell::Get(int32_t* aMcc, int32_t* aMnc, int32_t* aLac, int32_t* aCid,
                    int32_t* aAreaKnown) const
    {
    if (!iHaveResult)
        return KErrNotReady;
    if (iLastStatus != KErrNone)
        return iLastStatus;

    TInt mcc = 0;
    TInt mnc = 0;
    /* A failed parse is refused rather than reported as zero. MCC 0 is not a country and MNC 0 is
     * not an operator, but a caller that got them would happily build a query out of them and get
     * a confident answer about nowhere. */
    const TInt mccErr = ParseDigits(iInfo.iCountryCode, mcc);
    if (mccErr != KErrNone)
        return mccErr;
    const TInt mncErr = ParseDigits(iInfo.iNetworkId, mnc);
    if (mncErr != KErrNone)
        return mncErr;

    if (aMcc)
        *aMcc = mcc;
    if (aMnc)
        *aMnc = mnc;
    /* Reported even when iAreaKnown is false, with the flag beside them, so a diagnostic can show
     * what the modem actually said. A caller that skips the flag is the one at fault. */
    if (aLac)
        *aLac = (int32_t) iInfo.iLocationAreaCode;
    if (aCid)
        *aCid = (int32_t) iInfo.iCellId;
    if (aAreaKnown)
        *aAreaKnown = iInfo.iAreaKnown ? 1 : 0;
    return SHIM_OK;
    }

} /* namespace */

void ShimCellCleanup()
    {
    delete gCell;
    gCell = NULL;
    }

extern "C" {

int32_t shim_cell_read(void)
    {
    if (!gCell)
        {
        CShimCell* c = NULL;
        TInt err = KErrNone;
        TRAP(err, c = CShimCell::NewL());
        if (err != KErrNone)
            return err;
        if (!c)
            return SHIM_ERR_GENERAL;
        gCell = c;
        }
    gCell->Read();
    return SHIM_OK;
    }

int32_t shim_cell_get(int32_t* mcc, int32_t* mnc, int32_t* lac, int32_t* cid,
                      int32_t* area_known)
    {
    if (!gCell)
        return SHIM_ERR_NOT_READY;
    return gCell->Get(mcc, mnc, lac, cid, area_known);
    }

void shim_cell_stop(void)
    {
    ShimCellCleanup();
    }

} /* extern "C" */

#endif /* SHIM_USE_CELL */
