/* Audio playback, using Symbian's CMdaAudioPlayerUtility.
 *
 * One file in, sound out. The file must be in a format the platform's MMF plugins
 * recognise, and the list of those is short and fixed: AU, WAV and raw PCM are what
 * MMF ships as standard, with WAV covering IMA-ADPCM, A-law, mu-law, unsigned 8-bit
 * PCM, GSM 6.10 and signed 16-bit PCM. The device adds AMR, AAC and MP3.
 *
 * WHAT THIS DELIBERATELY DOES NOT DO: OPUS
 *
 * A Telegram voice message is Ogg/Opus, and no part of this device can open one.
 * `mmf/common/mmffourcc.h` lists every format code the framework knows and the list
 * ends at AMR, AAC, MP3, ATRAC3, SBC and WMA — Opus is from 2012 and the handset is
 * from 2008. So the Opus decode happens in Rust, off the GUI thread, and what arrives
 * here is a RIFF/WAVE file of signed little-endian 16-bit PCM that the caller wrote
 * itself. That is the format the MMF plugin resolver detects by the header signature
 * `RIFF????WAVE`, which is why a container is worth the 44 bytes: raw PCM is the one
 * standard format the resolver explicitly *cannot* identify, and using it would force
 * the far more awkward OpenUrlL path.
 *
 * WHY THERE IS NO ACTIVE OBJECT HERE, UNLIKE EVERY OTHER ASYNC MODULE IN THIS SHIM
 *
 * This looks like an omission and is not. CMdaAudioPlayerUtility already owns active
 * objects in the calling thread — that is why it panics E32USER-CBase 44 without a
 * scheduler — and it delivers its completions through MMdaAudioPlayerCallback rather
 * than through a TRequestStatus anyone else could wait on. Both of the SDK's shipped
 * audio examples (`sdk/s60cppexamples/CLFExample`, `sdk/s60cppexamples/AudioStreamExample`)
 * own exactly zero active objects of their own. Adding one here would give it nothing
 * to wait for.
 *
 * The real work — parsing the container, driving the codec — happens in a separate
 * controller *subthread* that MMF creates, not on our scheduler. This matters after
 * what happened to the image decoder: the shim's CIdle pump, being permanently ready,
 * starved every active object added at its priority, and the ICL's plugin AOs were in
 * our thread so they starved. MMF's decode is not, so it cannot be starved the same
 * way. What can still be delayed is delivery of our callbacks, and that is already
 * fixed by the pump sitting at EPriorityIdle - 1.
 *
 * CAPABILITIES: NONE
 *
 * Playback needs no capability. The SDK's capability report lists UserEnvironment
 * against recording only (CMdaAudioInputStream, CMMFDevSound::RecordInitL), and the
 * shipped CLFExample plays audio under `capability none`. MultimediaDD appears against
 * the priority-taking overloads, but its own documentation says it grants *precedence*
 * over processes that lack it rather than access — so a self-signed build plays fine
 * and merely loses arbitration against the ringtone.
 */

#include "shim_priv.h"
#include <mdaaudiosampleplayer.h>
#include <mmf/common/mmfbase.h>

/* One player, not a table of handles.
 *
 * The other async modules here hand out generation-tagged handles because a caller can
 * genuinely want four sockets or four decodes at once. Sound is not like that: the
 * device is a single exclusive resource, the platform arbitrates access to it between
 * processes by priority, and a second CMdaAudioPlayerUtility in one process is what
 * `faqSDK/faq_0726.html` describes failing with KErrInUse. Playing two voice messages
 * simultaneously is not a feature anyone asked for and not one the hardware offers.
 *
 * The generation counter survives anyway, so that a completion belonging to a clip the
 * user already dismissed can be recognised and dropped rather than acted on. */
class CShimPlayer : public CBase, public MMdaAudioPlayerCallback
    {
public:
    static CShimPlayer* NewL();
    ~CShimPlayer();

    TInt OpenFile(const TDesC& aPath);
    void Play();
    void Pause();
    void Stop();
    TInt PositionMs() const;
    TInt SetVolumePercent(TInt aPercent);
    TInt DurationMs() const { return iDurationMs; }
    TInt Generation() const { return iGeneration; }

    /* Callbacks. */
    void MapcInitComplete(TInt aError, const TTimeIntervalMicroSeconds& aDuration);
    void MapcPlayComplete(TInt aError);

private:
    void ConstructL();

    CMdaAudioPlayerUtility* iPlayer;
    TInt iDurationMs;
    /* Bumped on every open, so an event from a previous clip is identifiable. */
    TInt iGeneration;
    /* Whether a clip is open and playable. Tracked here rather than asked of the
     * utility because there is no state accessor, and because Stop() has to be treated
     * as terminal at its call site — see Stop(). */
    TBool iOpen;
    TBool iPlaying;
    };

static CShimPlayer* gPlayer = NULL;

CShimPlayer* CShimPlayer::NewL()
    {
    CShimPlayer* self = new (ELeave) CShimPlayer;
    CleanupStack::PushL(self);
    self->ConstructL();
    CleanupStack::Pop(self);
    return self;
    }

void CShimPlayer::ConstructL()
    {
    /* NewL rather than NewFilePlayerL, because the file arrives later and because this
     * is the overload both shipped examples use. The default priority arguments
     * (EMdaPriorityNormal, EMdaPriorityPreferenceTimeAndQuality) apply. */
    iPlayer = CMdaAudioPlayerUtility::NewL(*this);

    /* The default preference is TimeAndQuality, which is documented to *fail* if the
     * clip cannot be played immediately at full quality — so a ringtone or an alarm
     * holding the device turns into a voice message that silently never plays. Time
     * alone permits degraded output instead, which for speech is the right trade and is
     * what AudioStreamExample asks for. The return code is ignored on purpose: this
     * overload is annotated MultimediaDD, a capability a self-signed build will not
     * have, and failing to improve the preference is not a reason to refuse to play. */
    (void)iPlayer->SetPriority(EMdaPriorityNormal, EMdaPriorityPreferenceTime);
    }

CShimPlayer::~CShimPlayer()
    {
    if (iPlayer)
        {
        if (iPlaying)
            iPlayer->Stop();
        iPlayer->Close();
        delete iPlayer;
        iPlayer = NULL;
        }
    }

TInt CShimPlayer::OpenFile(const TDesC& aPath)
    {
    if (!iPlayer)
        return KErrNotReady;

    /* Whatever was open before is finished with. OpenFileL leaves with KErrInUse or
     * KErrNotReady if a previous open is still awaiting its completion, so closing
     * first is what makes opening a second clip work at all. */
    if (iPlaying)
        iPlayer->Stop();
    iPlayer->Close();
    iPlaying = EFalse;
    iOpen = EFalse;
    iDurationMs = 0;
    iGeneration++;

    TRAPD(err, iPlayer->OpenFileL(aPath));
    return err;
    }

void CShimPlayer::MapcInitComplete(TInt aError, const TTimeIntervalMicroSeconds& aDuration)
    {
    iOpen = (aError == KErrNone);
    /* Microseconds as a 64-bit count; milliseconds fit an int for any voice message,
     * and anything long enough to overflow is not something this device would play. */
    iDurationMs = iOpen ? static_cast<TInt>(aDuration.Int64() / 1000) : 0;

    ShimEvent e;
    e.kind = SHIM_EV_AUDIO_OPENED;
    e.handle = iGeneration;
    e.status = aError;
    e.a = iDurationMs;
    e.b = 0;
    e.c = 0;
    e.d = 0;
    e.native = 0;
    ShimPushEvent(e);
    }

void CShimPlayer::Play()
    {
    if (!iPlayer || !iOpen)
        return;
    /* Calling Play while already playing does not fail here — it reports KErrNotReady
     * through MapcPlayComplete, which would read as the clip having ended. */
    if (iPlaying)
        return;
    iPlaying = ETrue;
    iPlayer->Play();
    }

void CShimPlayer::Pause()
    {
    if (!iPlayer || !iPlaying)
        return;
    /* Pause keeps the position, so a following Play resumes rather than restarts. */
    (void)iPlayer->Pause();
    iPlaying = EFalse;
    }

void CShimPlayer::Stop()
    {
    if (!iPlayer || !iOpen)
        return;
    iPlayer->Stop();

    /* Stop does NOT deliver MapcPlayComplete — the reference is explicit about it. A
     * state machine that waits for the callback after stopping waits forever, with no
     * error to show for it, so the state changes here at the call site. Both shipped
     * examples do exactly this. */
    iPlaying = EFalse;

    ShimEvent e;
    e.kind = SHIM_EV_AUDIO_DONE;
    e.handle = iGeneration;
    e.status = KErrCancel;
    e.a = 0;
    e.b = 0;
    e.c = 0;
    e.d = 0;
    e.native = 0;
    ShimPushEvent(e);
    }

void CShimPlayer::MapcPlayComplete(TInt aError)
    {
    iPlaying = EFalse;

    /* Three codes mean "the clip ended", not "something went wrong". KErrUnderflow and
     * KErrOverflow/KErrEof are how MMF reports reaching the end of the data, and
     * treating them as failures turns every successful playback into an error message.
     * Everything else is a real failure — notably KErrInUse, which the header warns can
     * arrive *during* playback when a higher-priority client takes the device. */
    const TInt status =
        (aError == KErrUnderflow || aError == KErrOverflow || aError == KErrEof)
            ? KErrNone
            : aError;

    ShimEvent e;
    e.kind = SHIM_EV_AUDIO_DONE;
    e.handle = iGeneration;
    e.status = status;
    e.a = 0;
    e.b = 0;
    e.c = 0;
    /* The raw code is kept alongside the normalised one: "ended" and "ended by
     * underflow" are the same outcome to a caller and different things to whoever is
     * reading a probe report. */
    e.d = aError;
    e.native = 0;
    ShimPushEvent(e);
    }

TInt CShimPlayer::SetVolumePercent(TInt aPercent)
    {
    /* Both SetVolume and MaxVolume are documented to *panic* — not return an error —
     * when the utility is not initialised (EMMFMediaClientBadArgument and
     * EMMFMediaClientServerCommunicationProblem respectively). So the open check here
     * is not defensive tidiness; it is the difference between a silent no-op and
     * killing the process. */
    if (!iPlayer || !iOpen)
        return KErrNotReady;
    const TInt max = iPlayer->MaxVolume();
    if (max <= 0)
        return KErrNotSupported;
    return iPlayer->SetVolume((max * aPercent) / 100);
    }

TInt CShimPlayer::PositionMs() const
    {
    if (!iPlayer || !iOpen)
        return 0;
    TTimeIntervalMicroSeconds pos;
    if (iPlayer->GetPosition(pos) != KErrNone)
        return 0;
    return static_cast<TInt>(pos.Int64() / 1000);
    }

/* ------------------------------------------------------------------------ ABI -- */

static TInt EnsurePlayer()
    {
    if (gPlayer)
        return KErrNone;
    TRAPD(err, gPlayer = CShimPlayer::NewL());
    return err;
    }

extern "C" {

int32_t shim_audio_open_file(const uint16_t* aPath, int32_t aLen)
    {
    if (!aPath || aLen <= 0)
        return SHIM_ERR_ARGUMENT;
    const TInt err = EnsurePlayer();
    if (err != KErrNone)
        return err;
    TPtrC path(reinterpret_cast<const TUint16*>(aPath), aLen);
    return gPlayer->OpenFile(path);
    }

int32_t shim_audio_play()
    {
    if (!gPlayer)
        return SHIM_ERR_NOT_READY;
    gPlayer->Play();
    return SHIM_OK;
    }

int32_t shim_audio_pause()
    {
    if (!gPlayer)
        return SHIM_ERR_NOT_READY;
    gPlayer->Pause();
    return SHIM_OK;
    }

int32_t shim_audio_stop()
    {
    if (!gPlayer)
        return SHIM_ERR_NOT_READY;
    gPlayer->Stop();
    return SHIM_OK;
    }

/* Milliseconds into the clip, or 0 when nothing is open. Polled rather than pushed:
 * a position event per frame would cost more than reading it when a progress bar is
 * actually being drawn. */
int32_t shim_audio_position_ms()
    {
    return gPlayer ? gPlayer->PositionMs() : 0;
    }

int32_t shim_audio_duration_ms()
    {
    return gPlayer ? gPlayer->DurationMs() : 0;
    }

/* Volume as a percentage of the device maximum, clamped. The scale MaxVolume returns
 * is device-specific and not a percentage, so the conversion belongs on this side. */
int32_t shim_audio_set_volume(int32_t aPercent)
    {
    if (!gPlayer)
        return SHIM_ERR_NOT_READY;
    if (aPercent < 0) aPercent = 0;
    if (aPercent > 100) aPercent = 100;
    return gPlayer->SetVolumePercent(aPercent);
    }

int32_t shim_audio_close()
    {
    delete gPlayer;
    gPlayer = NULL;
    return SHIM_OK;
    }

} /* extern "C" */

void ShimAudioCleanup()
    {
    delete gPlayer;
    gPlayer = NULL;
    }
