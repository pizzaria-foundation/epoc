/* A text editor that exists only so the FEP will talk to us.
 *
 * THE PROBLEM
 *
 * The E72's Fn layer produces nothing. Fn+Q gives 'q' where it should give '!', and the
 * whole symbol row is unreachable — which for a Telegram client means a two-factor password
 * cannot be typed.
 *
 * The keyboard driver does not do that mapping. On S60 the front-end processor does, and
 * CAknFepManager only involves itself when the focused control advertises a FEP-aware text
 * editor. Without one it passes keys through untransformed, which is exactly the symptom.
 *
 * So this is the smallest object that satisfies MCoeFepAwareTextEditor, plus the
 * InputCapabilities override in shim_app.cpp that points at it.
 *
 * WHAT IT DELIBERATELY IS NOT
 *
 * It is not the editor. `symbian_ui::TextField` on the Rust side owns the real text, the
 * cursor and the layout, and nothing above the shim changes.
 *
 * This holds a 64-character scratch buffer that exists to satisfy the interface. The FEP
 * writes a transformed character into it, DoCommitFepInlineEditL pushes that character to
 * Rust as an ordinary key event, and the scratch is cleared. Reporting a document length of
 * zero would be simpler and is wrong: the FEP asks before it decides what to do, and a
 * length that never changes makes multitap loop on the same character.
 *
 * TWO PATHS SHIP, AND ONE DEVICE RUN CHOOSES
 *
 * The existing scan-code translation stays and is selectable at run time through
 * shim_keyboard_mode. Six rounds went into the bearer because each build tested one guess;
 * this ships both and lets one report compare them. It also means a FEP that turns out not
 * to fire leaves a working keyboard rather than none.
 *
 * WHAT IS NOT KNOWN FROM HERE
 *
 * Whether CAknFepManager delivers a transformed key as an ordinary EEventKey once an editor
 * is present, or drives it through StartFepInlineEditL and DoCommitFepInlineEditL. Both are
 * handled. On a QWERTY handset with no predictive text the first is likely and the second
 * is what multitap uses, and nothing on this side of the wire can settle it.
 */

#include "symbian_shim.h"
#include "shim_priv.h"

#include <e32base.h>
#include <fepbase.h>
#include <frmtlay.h>
#include <eikdgfty.h>

/* Longest inline edit this will hold.
 *
 * Predictive text composes a word before committing it, so this is not one character. Sixty
 * four is past the longest word in any language the handset ships with, and the buffer is
 * a member rather than a heap allocation because it is written from a FEP callback that
 * cannot be allowed to fail. */
const TInt KMaxInline = 64;

class CShimFepEditor : public MCoeFepAwareTextEditor,
                       public MCoeFepAwareTextEditor_Extension1
    {
public:
    CShimFepEditor();
    ~CShimFepEditor();

    /* MCoeFepAwareTextEditor. Twelve pure virtuals; most of them answer a question about a
     * document that does not exist here, and each says what it is answering and why that
     * answer is safe. */
    void StartFepInlineEditL(const TDesC& aInitialInlineText,
                             TInt aPositionOfInsertionPointInInlineText,
                             TBool aCursorVisibility,
                             const MFormCustomDraw* aCustomDraw,
                             MFepInlineTextFormatRetriever& aInlineTextFormatRetriever,
                             MFepPointerEventHandlerDuringInlineEdit& aPointerEventHandler);
    void UpdateFepInlineTextL(const TDesC& aNewInlineText,
                              TInt aPositionOfInsertionPointInInlineText);
    void SetInlineEditingCursorVisibilityL(TBool aCursorVisibility);
    void CancelFepInlineEdit();
    TInt DocumentLengthForFep() const;
    TInt DocumentMaximumLengthForFep() const;
    void SetCursorSelectionForFepL(const TCursorSelection& aCursorSelection);
    void GetCursorSelectionForFep(TCursorSelection& aCursorSelection) const;
    void GetEditorContentForFep(TDes& aEditorContent, TInt aDocumentPosition,
                                TInt aLengthToRetrieve) const;
    void GetFormatForFep(TCharFormat& aFormat, TInt aDocumentPosition) const;
    void GetScreenCoordinatesForFepL(TPoint& aLeftSideOfBaseLine, TInt& aHeight,
                                     TInt& aAscent, TInt aDocumentPosition) const;
    void DoCommitFepInlineEditL();

    /* MCoeFepAwareTextEditor_Extension1. The FEP creates the state object and hands it
     * over; the editor stores it and gives it back. That direction is worth noticing --
     * it means no Avkon type is constructed here and no avkon ordinal is imported for it. */
    void SetStateTransferingOwnershipL(CState* aState, TUid aTypeSafetyUid);
    CState* State(TUid aTypeSafetyUid);

private:
    MCoeFepAwareTextEditor_Extension1* Extension1(TBool& aSetToTrue);

    void PushCommitted();

    /* The composition in progress. Not the document -- see the file comment. */
    TBuf<KMaxInline> iInline;
    /* Where the FEP thinks the insertion point is within iInline. */
    TInt iCursor;
    TBool iEditing;

    /* Owned. The FEP transfers it and expects to get the same pointer back. */
    CState* iState;
    TUid iStateUid;
    };

CShimFepEditor* gFepEditor = NULL;

/* Which mechanism is live.
 *
 * Both are compiled in. SHIM_KEYBOARD_FEP advertises the editor below; SHIM_KEYBOARD_SCAN
 * is the scan-code table that is tested on hardware and works for letters and digits.
 *
 * When SHIM_USE_FEP is defined at compile time, the editor is always created and the
 * default is FEP — no Rust-side call needed. A call to shim_keyboard_mode(SHIM_KEYBOARD_SCAN)
 * can still switch back at runtime for comparison. */
#ifdef SHIM_USE_FEP
TInt gKeyboardMode = SHIM_KEYBOARD_FEP;
#else
TInt gKeyboardMode = SHIM_KEYBOARD_SCAN;
#endif

CShimFepEditor::CShimFepEditor()
    : iCursor(0), iEditing(EFalse), iState(NULL), iStateUid(TUid::Null())
    {
    }

CShimFepEditor::~CShimFepEditor()
    {
    delete iState;
    }

MCoeFepAwareTextEditor_Extension1* CShimFepEditor::Extension1(TBool& aSetToTrue)
    {
    /* Returning NULL here -- the default -- is what stops Avkon's FEP configuring itself
     * for this editor at all. The out parameter is how the framework tells an old editor
     * that did not override this from a new one that returned NULL on purpose. */
    aSetToTrue = ETrue;
    return this;
    }

void CShimFepEditor::SetStateTransferingOwnershipL(CState* aState, TUid aTypeSafetyUid)
    {
    /* "Transferring ownership" means the old one is ours to delete. Leaking it would be a
     * slow leak per focus change rather than a crash, which is the kind that gets found
     * three weeks later as "the phone gets slow". */
    delete iState;
    iState = aState;
    iStateUid = aTypeSafetyUid;
    }

MCoeFepAwareTextEditor_Extension1::CState* CShimFepEditor::State(TUid aTypeSafetyUid)
    {
    /* The uid check is the point of the parameter: the FEP downcasts what comes back, so
     * handing a different FEP's state object to this one is a wild cast. */
    return aTypeSafetyUid == iStateUid ? iState : NULL;
    }

void CShimFepEditor::StartFepInlineEditL(const TDesC& aInitialInlineText,
                                         TInt aPositionOfInsertionPointInInlineText,
                                         TBool /*aCursorVisibility*/,
                                         const MFormCustomDraw* /*aCustomDraw*/,
                                         MFepInlineTextFormatRetriever& /*aFormat*/,
                                         MFepPointerEventHandlerDuringInlineEdit& /*aPtr*/)
    {
    iEditing = ETrue;
    iInline = aInitialInlineText.Left(KMaxInline);
    iCursor = aPositionOfInsertionPointInInlineText;
    }

void CShimFepEditor::UpdateFepInlineTextL(const TDesC& aNewInlineText,
                                          TInt aPositionOfInsertionPointInInlineText)
    {
    iInline = aNewInlineText.Left(KMaxInline);
    iCursor = aPositionOfInsertionPointInInlineText;
    }

void CShimFepEditor::SetInlineEditingCursorVisibilityL(TBool /*aCursorVisibility*/)
    {
    /* Rust draws its own cursor and does not know this one exists. Nothing to do, and
     * nothing that can go wrong by doing nothing. */
    }

void CShimFepEditor::CancelFepInlineEdit()
    {
    /* Abandoned rather than committed: the user pressed something that ended the
     * composition without accepting it. Pushing the characters here would type a word
     * someone decided not to type. */
    iInline.Zero();
    iCursor = 0;
    iEditing = EFalse;
    }

TInt CShimFepEditor::DocumentLengthForFep() const
    {
    /* The composition, not the document.
     *
     * Rust owns the real text and the shim cannot see it. Reporting the true length would
     * mean mirroring every keystroke across the boundary for a number the FEP uses only to
     * bound a cursor -- and the FEP's cursor lives inside the composition, not the
     * document. Reporting zero unconditionally is what breaks multitap: the FEP compares
     * this before and after to decide whether its own edit landed. */
    return iInline.Length();
    }

TInt CShimFepEditor::DocumentMaximumLengthForFep() const
    {
    return KMaxInline;
    }

void CShimFepEditor::SetCursorSelectionForFepL(const TCursorSelection& aCursorSelection)
    {
    iCursor = aCursorSelection.iCursorPos;
    }

void CShimFepEditor::GetCursorSelectionForFep(TCursorSelection& aCursorSelection) const
    {
    /* Anchor equal to cursor: an empty selection. A non-empty one would tell the FEP there
     * is text to replace, and it would replace text this object does not have. */
    aCursorSelection.iCursorPos = iCursor;
    aCursorSelection.iAnchorPos = iCursor;
    }

void CShimFepEditor::GetEditorContentForFep(TDes& aEditorContent, TInt aDocumentPosition,
                                            TInt aLengthToRetrieve) const
    {
    aEditorContent.Zero();
    if (aDocumentPosition < 0 || aDocumentPosition > iInline.Length())
        return;
    TInt avail = iInline.Length() - aDocumentPosition;
    TInt take = aLengthToRetrieve < avail ? aLengthToRetrieve : avail;
    if (take > aEditorContent.MaxLength())
        take = aEditorContent.MaxLength();
    if (take > 0)
        aEditorContent.Copy(iInline.Mid(aDocumentPosition, take));
    }

void CShimFepEditor::GetFormatForFep(TCharFormat& /*aFormat*/, TInt /*aDocumentPosition*/) const
    {
    /* Left as the caller constructed it, deliberately.
     *
     * The FEP asks so it can draw the composition in the editor's own font. It will not get
     * to: Rust draws everything, and the underline the FEP would apply is not something the
     * canvas knows about.
     *
     * Assigning a default-constructed TCharFormat was the first version and does not link:
     * its constructor lives in etext.dso, which this shim has no other reason to import.
     * The parameter arrives already constructed, so leaving it alone is both a valid answer
     * and one fewer library. */
    }

void CShimFepEditor::GetScreenCoordinatesForFepL(TPoint& aLeftSideOfBaseLine, TInt& aHeight,
                                                 TInt& aAscent, TInt /*aDocumentPosition*/) const
    {
    /* Where to put a candidate popup. Zero and a plausible line height rather than a real
     * position, because the real one is inside a Rust layout the shim cannot query -- and
     * a popup in the wrong place is a cosmetic problem, while leaving this unimplemented
     * would be a panic in the FEP. */
    aLeftSideOfBaseLine = TPoint(0, 0);
    aHeight = 16;
    aAscent = 12;
    }

void CShimFepEditor::DoCommitFepInlineEditL()
    {
    PushCommitted();
    iInline.Zero();
    iCursor = 0;
    iEditing = EFalse;
    }

void CShimFepEditor::PushCommitted()
    {
    /* One event per character, through the same path an ordinary keypress takes.
     *
     * That is what keeps the Rust side unchanged: `symbian_ui::TextField` sees a key event
     * with a character code and does not know or care that a front-end processor composed
     * it. A composition of five characters arrives as five keypresses, which is what the
     * user typed. */
    for (TInt i = 0; i < iInline.Length(); i++)
        {
        /* SHIM_EV_KEY_CHAR, not KEY_DOWN: this is a translated character, which is
         * exactly what that event means and exactly what the FEP produced. */
        ShimPushSimple(SHIM_EV_KEY_CHAR, 0, static_cast<TInt>(iInline[i]), 0);
        }
    }

/* ------------------------------------------------------------------ the shim's side -- */

MCoeFepAwareTextEditor* ShimFepEditor()
    {
    /* Created on demand and never destroyed before ShimFepCleanup. InputCapabilities() is
     * const and is called from the framework's own traversal, so it cannot allocate -- the
     * editor has to exist by then. */
    return (gKeyboardMode == SHIM_KEYBOARD_FEP) ? gFepEditor : NULL;
    }

void ShimFepInit()
    {
    if (!gFepEditor)
        gFepEditor = new CShimFepEditor;
    }

void ShimFepCleanup()
    {
    delete gFepEditor;
    gFepEditor = NULL;
    }

extern "C" {

int32_t shim_keyboard_mode(int32_t mode)
    {
    if (mode != SHIM_KEYBOARD_SCAN && mode != SHIM_KEYBOARD_FEP)
        return SHIM_ERR_ARGUMENT;
    if (mode == SHIM_KEYBOARD_FEP && !gFepEditor)
        return SHIM_ERR_NOT_READY;
    gKeyboardMode = mode;
    return SHIM_OK;
    }

int32_t shim_keyboard_mode_get(void)
    {
    return gKeyboardMode;
    }

} /* extern "C" */
