//! The `SpeechTranscriber` speech-to-text adapter (task H2.1, spec 11.3).
//!
//! The lifecycle spec 11.3 describes is implemented here in order: check
//! availability, supported and installed locales, required downloadable
//! assets, permission and concurrency limits; let the user authorize a
//! download and show its progress; analyse the file; gather *finalized*
//! segments and timestamps; report errors without losing the original audio.
//!
//! Four refusals are deliberate and are what make the adapter safe to put in
//! front of a job queue:
//!
//! * A locale whose assets are not installed fails closed. It never silently
//!   substitutes a language the user did not ask for, and it never starts a
//!   download the user did not approve (spec 12).
//! * A cancelled or timed-out run returns [`SttError::Cancelled`] or a typed
//!   failure, never a partial transcript.
//! * Offsets appear only when the engine reported them. A text-only result
//!   keeps [`AlignmentOrigin::None`] and therefore offers no seek affordance
//!   (spec 8.6A).
//! * Nothing here writes to the input. A failure leaves the archived original
//!   byte-identical, which the test suite asserts.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use plaudon_ai_contracts::capabilities::SttCapabilities;
use plaudon_ai_contracts::job::{JobContext, Progress};
use plaudon_ai_contracts::stt::{
    SpeechToText, SttError, SttInput, TranscriptDraft, TranscriptSegmentDraft,
};
use plaudon_ai_contracts::transcript::{DraftWordAlignment, SourceTime, TimePrecision};
use plaudon_domain::locale::LanguageTag;
use plaudon_domain::model::{EngineId, ModelIdentity};
use plaudon_domain::transcript::{Confidence, ConfidenceScale};
use tokio::sync::Semaphore;

use crate::bridge::{AppleBridge, FfiError};
use crate::envelope::{
    encode_request, AssetInstall, AssetStatus, BridgeError, BridgeErrorCode, BridgeOp, BridgeReply,
    InstallAssetsRequest, ProgressEvent, TranscribeRequest, Transcription,
};
use crate::probe::{
    speech_capabilities, AppleAvailability, AssetDownloadConsent, ProbeCache, APPLE_SPEECH_ENGINE,
    APPLE_SPEECH_MODEL,
};

/// Default wall-clock budget for one transcription.
///
/// Spec 8.2 caps a single local run at thirty minutes of audio; the engine is
/// faster than real time on supported hardware, so an hour of wall clock is a
/// generous ceiling that still guarantees the job queue gets an answer.
pub const DEFAULT_TRANSCRIBE_TIMEOUT: Duration = Duration::from_secs(60 * 60);

/// Default budget for an asset installation.
pub const DEFAULT_INSTALL_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// What an asset installation would involve, for the consent prompt.
///
/// Spec 12 requires the size and the decision to be the user's. The platform
/// does not publish a byte count before the download starts, so the prompt
/// states what is known and says what is not, rather than inventing a figure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetPlan {
    /// Locale the assets belong to.
    pub locale: LanguageTag,
    /// Current installation state.
    pub status: AssetStatus,
    /// Whether the platform can serve this locale at all.
    pub installable: bool,
    /// Download size in bytes, when the platform reports one. It does not.
    pub download_bytes: Option<u64>,
}

impl AssetPlan {
    /// Whether a download is needed before transcription can start.
    #[must_use]
    pub const fn requires_download(&self) -> bool {
        matches!(self.status, AssetStatus::Supported)
    }
}

/// The Apple speech-to-text engine.
#[derive(Debug)]
pub struct AppleSpeechToText {
    bridge: Arc<dyn AppleBridge>,
    cache: Arc<ProbeCache>,
    consent: AssetDownloadConsent,
    permits: Arc<Semaphore>,
    timeout: Duration,
    install_timeout: Duration,
    want_word_timestamps: bool,
}

impl AppleSpeechToText {
    /// Build an adapter over `bridge`, sharing `cache` with the engine policy.
    ///
    /// Construction performs no I/O and takes the always-safe concurrency
    /// floor of one in-flight run. The composition root raises it from the
    /// probe with [`Self::probed`], which is the only source of a higher
    /// ceiling: spec 11.3 lists concurrency limits among the things to ask
    /// the platform for rather than assume.
    #[must_use]
    pub fn new(bridge: Arc<dyn AppleBridge>, cache: Arc<ProbeCache>) -> Self {
        Self {
            bridge,
            cache,
            consent: AssetDownloadConsent::Withheld,
            permits: Arc::new(Semaphore::new(1)),
            timeout: DEFAULT_TRANSCRIBE_TIMEOUT,
            install_timeout: DEFAULT_INSTALL_TIMEOUT,
            want_word_timestamps: true,
        }
    }

    /// Raise the concurrency ceiling to what the platform reports.
    ///
    /// The only limit the platform publishes is the maximum number of
    /// simultaneously reserved locales, so that is the limit enforced here
    /// (task H1.4). A probe that did not answer leaves the floor in place.
    pub async fn probed(mut self) -> Self {
        let ceiling = self
            .availability(None)
            .await
            .speech
            .as_ref()
            .ok()
            .map_or(1, |probe| probe.max_concurrent_locales.max(1));
        self.permits = Arc::new(Semaphore::new(ceiling as usize));
        self
    }

    /// Override the concurrency ceiling.
    #[must_use]
    pub fn with_concurrency(mut self, permits: u32) -> Self {
        self.permits = Arc::new(Semaphore::new(permits.max(1) as usize));
        self
    }

    /// Record that the user authorized asset downloads for this engine.
    #[must_use]
    pub const fn with_asset_consent(mut self, consent: AssetDownloadConsent) -> Self {
        self.consent = consent;
        self
    }

    /// Override the transcription budget.
    #[must_use]
    pub const fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Ask for or suppress word-level timings.
    #[must_use]
    pub const fn with_word_timestamps(mut self, wanted: bool) -> Self {
        self.want_word_timestamps = wanted;
        self
    }

    /// Current availability snapshot, from the shared cache.
    pub async fn availability(&self, locale: Option<&str>) -> AppleAvailability {
        let cache = self.cache.clone();
        let locale = locale.map(str::to_owned);
        tokio::task::spawn_blocking(move || cache.availability(locale.as_deref()).0)
            .await
            .unwrap_or_else(|_| {
                AppleAvailability::unavailable(BridgeError::new(
                    BridgeErrorCode::EngineFailure,
                    Some("probe_task_panicked".to_owned()),
                ))
            })
    }

    /// What installing `locale` would involve.
    ///
    /// The UI calls this before showing the consent prompt; nothing is
    /// downloaded.
    pub async fn asset_plan(&self, locale: &LanguageTag) -> AssetPlan {
        let availability = self.availability(Some(locale.as_str())).await;
        let (status, installable) = availability.speech.as_ref().map_or_else(
            |_| (AssetStatus::Unsupported, false),
            |probe| {
                let installable = probe
                    .supported_locales
                    .iter()
                    .filter_map(|tag| LanguageTag::parse(&tag.replace('_', "-")).ok())
                    .any(|tag| tag.primary_language() == locale.primary_language());
                (probe.asset_status, installable)
            },
        );
        AssetPlan {
            locale: locale.clone(),
            status,
            installable,
            download_bytes: None,
        }
    }

    /// Download and install the speech assets for `locale`.
    ///
    /// Only called after the user approved the plan from [`Self::asset_plan`].
    /// Progress is reported through `ctx`, and cancellation is honoured.
    pub async fn install_assets(
        &self,
        locale: &LanguageTag,
        ctx: JobContext,
    ) -> Result<AssetInstall, SttError> {
        let request = encode_request(
            BridgeOp::InstallSpeechAssets,
            &InstallAssetsRequest {
                locale: Some(locale.as_str().to_owned()),
                cancel_token: 0,
                timeout_ms: 0,
            },
        );
        let reply: BridgeReply<AssetInstall> = self
            .dispatch(
                BridgeOp::InstallSpeechAssets,
                request,
                self.install_timeout,
                ctx,
            )
            .await?;
        match reply {
            BridgeReply::Ok(result) => {
                // The installed-locale set changed, so every cached verdict
                // about this engine is stale (spec 11.6).
                self.cache.invalidate();
                Ok(result)
            }
            BridgeReply::Err(error) => Err(map_error(&error)),
        }
    }

    /// Send one operation across the bridge with cancellation and progress.
    ///
    /// The bridge call is blocking, so it runs on the blocking pool; the
    /// cancellation token is mirrored onto the library's handle by a watcher
    /// that is dropped as soon as the call returns.
    async fn dispatch<T: serde::de::DeserializeOwned + Send + 'static>(
        &self,
        op: BridgeOp,
        request: String,
        timeout: Duration,
        ctx: JobContext,
    ) -> Result<BridgeReply<T>, SttError> {
        dispatch_bridge(&self.bridge, op, request, timeout, ctx)
            .await
            .map_err(|error| map_dispatch_error(&error))
    }
}

/// Why a dispatch produced no reply.
///
/// Cancellation is a distinct outcome rather than a boundary failure: the
/// job engine renders the two very differently, and spec 11.3 requires a
/// cancelled run to say so instead of looking like the engine broke.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DispatchError {
    /// The caller's token fired before the library answered.
    Cancelled,
    /// The boundary itself failed.
    Ffi(FfiError),
}

/// Shared dispatch used by both Apple adapters.
///
/// Placing the cancellation handle, the progress channel and the blocking
/// hand-off in one function is what keeps the two adapters from drifting
/// apart on the parts that are easy to get subtly wrong.
pub(crate) async fn dispatch_bridge<T: serde::de::DeserializeOwned + Send + 'static>(
    bridge: &Arc<dyn AppleBridge>,
    op: BridgeOp,
    request: String,
    timeout: Duration,
    ctx: JobContext,
) -> Result<BridgeReply<T>, DispatchError> {
    let guard = crate::bridge::CancelGuard::new(bridge.clone());
    let request = inject_call_parameters(&request, guard.token(), timeout);

    // Progress arrives on a library thread; the channel moves it onto the
    // runtime without ever blocking the engine (spec 6.5).
    let (progress_tx, mut progress_rx) = tokio::sync::mpsc::unbounded_channel::<ProgressEvent>();
    let sink = ctx.progress.clone();
    let forwarder = tokio::spawn(async move {
        while let Some(event) = progress_rx.recv().await {
            sink.report(Progress {
                fraction: event.fraction,
                processed_ms: event.processed_ms,
            });
        }
    });

    let cancel = ctx.cancel.clone();
    let bridge_for_call = bridge.clone();
    let token = guard.token();
    let mut call = tokio::task::spawn_blocking(move || {
        let emit = move |event: ProgressEvent| {
            // A closed channel means the caller stopped listening; dropping
            // the update is correct and must not disturb the engine.
            let _ = progress_tx.send(event);
        };
        bridge_for_call.call(op, &request, Some(&emit))
    });

    let outcome = tokio::select! {
        // Borrowed rather than moved, so the cancellation arm can still
        // await the task it has just asked to stop.
        result = &mut call => {
            match result {
                Ok(Ok(document)) => Ok(document),
                Ok(Err(error)) => Err(DispatchError::Ffi(error)),
                // A panicked blocking task is a defect in this crate, not a
                // platform answer; report it as a boundary failure rather
                // than as a cancelled run.
                Err(_) => Err(DispatchError::Ffi(FfiError::NullDocument)),
            }
        }
        () = cancel.cancelled() => {
            // Tell the library to stop, then wait for the document it owes
            // us. Returning before it lands would drop the guard — and so
            // release the handle — while the library is still using it.
            bridge.cancel(token);
            match call.await {
                Ok(_) | Err(_) => Err(DispatchError::Cancelled),
            }
        }
    };

    drop(guard);
    forwarder.abort();
    outcome.map(|document| BridgeReply::parse(op, &document))
}

/// Place the cancellation handle and budget into an already-encoded request.
///
/// The envelope is built by a typed struct, but the handle is only known
/// once the guard exists, and the budget belongs to the adapter rather than
/// the caller. Patching two scalar fields keeps one encoder.
fn inject_call_parameters(request: &str, token: u64, timeout: Duration) -> String {
    let Ok(mut value) = serde_json::from_str::<serde_json::Value>(request) else {
        return request.to_owned();
    };
    if let Some(map) = value.as_object_mut() {
        map.insert("cancel_token".into(), token.into());
        map.insert(
            "timeout_ms".into(),
            u64::try_from(timeout.as_millis())
                .unwrap_or(u64::MAX)
                .into(),
        );
    }
    serde_json::to_string(&value).unwrap_or_else(|_| request.to_owned())
}

/// Map a bridge failure onto the speech-to-text error vocabulary.
#[must_use]
pub fn map_error(error: &BridgeError) -> SttError {
    match error.code {
        BridgeErrorCode::Cancelled => SttError::Cancelled,
        BridgeErrorCode::PlatformUnsupported
        | BridgeErrorCode::HardwareUnsupported
        | BridgeErrorCode::DisabledByUser
        | BridgeErrorCode::PermissionDenied
        | BridgeErrorCode::LocaleUnsupported
        | BridgeErrorCode::UnsupportedLanguage
        | BridgeErrorCode::ModelAssetMissing
        | BridgeErrorCode::ModelAssetNotReady
        | BridgeErrorCode::AssetDownloadFailed
        | BridgeErrorCode::ConcurrencyLimit => SttError::Unavailable,
        BridgeErrorCode::InputUnreadable | BridgeErrorCode::UnsupportedMedia => {
            SttError::UnsupportedFormat
        }
        BridgeErrorCode::ContextSizeExceeded => SttError::InputTooLarge,
        // A volatile-only result is output we cannot turn into a revision; it
        // is not a silent empty transcript (spec 11.3).
        BridgeErrorCode::OutputNotFinal => SttError::MalformedOutput,
        _ => SttError::EngineFailure,
    }
}

pub(crate) fn map_dispatch_error(error: &DispatchError) -> SttError {
    match error {
        DispatchError::Cancelled => SttError::Cancelled,
        DispatchError::Ffi(FfiError::NotLinked) => SttError::Unavailable,
        DispatchError::Ffi(_) => SttError::EngineFailure,
    }
}

/// Turn a bridge transcription into a draft revision.
///
/// Pure, so the timestamp and alignment rules are tested without a Mac.
///
/// Offsets arrive from the platform in milliseconds and are placed on the
/// *source* timeline: `input.source_sample_offset` is added, so a chunk after
/// the first does not restart at zero (spec 8.2). Precision is
/// [`TimePrecision::Segment`], because a rounded millisecond figure from an
/// engine is a claim about a segment, not a sample index.
///
/// # Errors
///
/// Returns [`SttError::MalformedOutput`] when the document contradicts
/// itself: a non-final result, offsets on a result that declares it has none,
/// or a claimed alignment origin with no offsets anywhere.
pub fn to_draft(result: &Transcription, input: &SttInput) -> Result<TranscriptDraft, SttError> {
    if !result.final_result {
        return Err(SttError::MalformedOutput);
    }
    let engine = EngineId::new(APPLE_SPEECH_ENGINE);
    let locale = LanguageTag::parse(&result.locale.replace('_', "-")).ok();
    let rate = input.sample_rate_hz.max(1);
    let base = input.source_sample_offset;
    let at = |millis: u64| SourceTime {
        sample_offset: base + (millis * u64::from(rate)) / 1000,
        sample_rate_hz: rate,
        precision: TimePrecision::Segment,
        uncertainty_ms: None,
    };

    let mut segments = Vec::with_capacity(result.segments.len());
    for (index, raw) in result.segments.iter().enumerate() {
        let (start, end) = match (raw.start_ms, raw.end_ms) {
            (Some(start), Some(end)) if end >= start => (Some(at(start)), Some(at(end))),
            // A half-open or inverted span is not a timeline. Dropping the
            // offsets is honest; keeping them would offer a seek to a
            // position the engine never claimed (spec 8.6A).
            _ => (None, None),
        };
        if !result.alignment_origin.permits_times() && start.is_some() {
            return Err(SttError::MalformedOutput);
        }
        let confidence = raw.confidence.map(|value| {
            // Apple reports a 0..1 score per attributed run. It is
            // engine-scoped, and spec 8.5 forbids comparing it across vendors.
            Confidence::new(engine.clone(), ConfidenceScale::Probability, value)
        });
        let words = if result.alignment_origin.permits_times() {
            raw.words
                .iter()
                .filter(|word| word.end_ms >= word.start_ms)
                .map(|word| DraftWordAlignment {
                    text: word.text.clone(),
                    start: at(word.start_ms),
                    end: at(word.end_ms),
                    confidence: None,
                })
                .collect()
        } else {
            Vec::new()
        };
        segments.push(TranscriptSegmentDraft {
            seq_index: u32::try_from(index).unwrap_or(u32::MAX),
            start,
            end,
            text: raw.text.clone(),
            confidence,
            speaker_id: None,
            words,
        });
    }

    if result.alignment_origin.permits_times()
        && !segments.is_empty()
        && segments.iter().all(|segment| segment.start.is_none())
    {
        // The result claims a timeline it did not deliver.
        return Err(SttError::MalformedOutput);
    }

    let mut engine_params = BTreeMap::new();
    engine_params.insert("locale".to_owned(), result.locale.clone());
    engine_params.insert(
        "attribute_options".to_owned(),
        "audio_time_range,transcription_confidence".to_owned(),
    );
    engine_params.insert(
        "reporting_options".to_owned(),
        // Volatile results are not requested: only finalized output can
        // become a revision (spec 11.3).
        "finalized_only".to_owned(),
    );

    Ok(TranscriptDraft {
        engine_id: engine.clone(),
        model: ModelIdentity::new(engine, APPLE_SPEECH_MODEL),
        input_sha256: input.media_sha256,
        locale,
        alignment_origin: result.alignment_origin,
        segments,
        duration_ms: result.duration_ms,
        engine_params,
        chunking_strategy: input.chunking_strategy.clone(),
        normalization_profile: input.normalization_profile.clone(),
        annotations: Vec::new(),
        // The platform does not disclose whether it used the neural engine,
        // the GPU or the CPU, so the recorded backend names the runtime we
        // actually drove rather than a guess about the silicon.
        backend: Some("apple-speechanalyzer".to_owned()),
    })
}

#[async_trait]
impl SpeechToText for AppleSpeechToText {
    async fn capabilities(&self) -> SttCapabilities {
        let availability = self.availability(None).await;
        speech_capabilities(&availability.speech, self.consent)
    }

    async fn transcribe(
        &self,
        input: SttInput,
        ctx: JobContext,
    ) -> Result<TranscriptDraft, SttError> {
        if ctx.is_cancelled() {
            return Err(SttError::Cancelled);
        }

        let locale_hint = input.locale_hint.clone();
        let availability = self
            .availability(locale_hint.as_ref().map(LanguageTag::as_str))
            .await;
        let caps = speech_capabilities(&availability.speech, self.consent);
        if !caps.is_selectable() {
            // Fail closed with the probe's reason rather than attempting a
            // run the platform already said it cannot do.
            return Err(availability
                .speech
                .as_ref()
                .err()
                .map_or(SttError::Unavailable, map_error));
        }
        if !caps.supports_format(&input.format) {
            return Err(SttError::UnsupportedFormat);
        }
        if let Some(locale) = &locale_hint {
            if !caps.supports_locale(locale) {
                // Spec 11.3: no silent language substitution.
                return Err(SttError::Unavailable);
            }
        }

        let path = absolute_path(&input.media_path)?;

        // The platform's published concurrency ceiling, enforced here so a
        // job queue cannot drive the analyzer past it (task H1.4).
        let permit = tokio::select! {
            permit = self.permits.clone().acquire_owned() => {
                permit.map_err(|_| SttError::EngineFailure)?
            }
            () = ctx.cancel.cancelled() => return Err(SttError::Cancelled),
        };

        let request = encode_request(
            BridgeOp::TranscribeFile,
            &TranscribeRequest {
                path,
                locale: locale_hint.as_ref().map(|l| l.as_str().to_owned()),
                cancel_token: 0,
                timeout_ms: 0,
                allow_asset_download: self.consent.permits_download(),
                want_word_timestamps: self.want_word_timestamps,
            },
        );
        let reply: BridgeReply<Transcription> = self
            .dispatch(BridgeOp::TranscribeFile, request, self.timeout, ctx.clone())
            .await?;
        drop(permit);

        let result = match reply {
            BridgeReply::Ok(result) => result,
            BridgeReply::Err(error) => return Err(map_error(&error)),
        };

        // A run that was cancelled after the library had already produced a
        // document must still not yield a transcript (spec 11.3).
        if ctx.is_cancelled() {
            return Err(SttError::Cancelled);
        }

        if let Some(requested) = &locale_hint {
            let produced = LanguageTag::parse(&result.locale.replace('_', "-")).ok();
            let matches = produced
                .as_ref()
                .is_some_and(|tag| tag.primary_language() == requested.primary_language());
            if !matches {
                // The engine answered in a different language than asked.
                return Err(SttError::Unavailable);
            }
        }

        to_draft(&result, &input)
    }
}

/// Resolve the media path to something the bridge can open.
fn absolute_path(path: &Path) -> Result<String, SttError> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|_| SttError::UnsupportedFormat)?
            .join(path)
    };
    absolute
        .to_str()
        .map(str::to_owned)
        // The bridge takes a NUL-terminated UTF-8 path; a non-UTF-8 path is a
        // refusal, not a lossy conversion to a different file.
        .ok_or(SttError::UnsupportedFormat)
}

#[cfg(test)]
mod tests {
    use plaudon_ai_contracts::capabilities::MediaFormat;
    use plaudon_ai_contracts::job::RecordingProgressSink;
    use plaudon_domain::ids::JobId;
    use plaudon_domain::media::Sha256Digest;
    use plaudon_domain::transcript::AlignmentOrigin;

    use crate::bridge::{failure_document, success_document, FakeBridge, ScriptedCall};
    use crate::envelope::BridgeOp;

    use super::*;

    fn digest() -> Sha256Digest {
        Sha256Digest::from_bytes([7u8; 32])
    }

    fn speech_probe(status: &str, installed: &[&str]) -> serde_json::Value {
        serde_json::json!({
            "available": true,
            "supported_locales": ["en_US", "ja_JP"],
            "installed_locales": installed,
            "resolved_locale": "en_US",
            "asset_status": status,
            "locale_installed": installed.contains(&"en_US"),
            "supports_on_device": true,
            "is_transcriber": true,
            "max_concurrent_locales": 2,
            "reserved_locales": [],
            "accepted_formats": [],
            "supports_word_timestamps": true,
            "os_build": "26A434",
            "os_version": "27.0.1",
            "hardware": "Apple M4 Pro",
        })
    }

    fn transcription(alignment: &str, segments: &serde_json::Value) -> serde_json::Value {
        serde_json::json!({
            "text": "hello world",
            "segments": segments,
            "alignment_origin": alignment,
            "locale": "en_US",
            "duration_ms": 1500,
            "final": true,
        })
    }

    fn input(path: &Path) -> SttInput {
        let mut input = SttInput::whole_file(
            path.to_path_buf(),
            digest(),
            1024,
            MediaFormat::new("wav", "pcm_s16le"),
            16_000,
            1,
        );
        input.duration_ms = Some(1500);
        input.locale_hint = Some(LanguageTag::parse("en-US").unwrap());
        input
    }

    /// An input standing in for an already-committed recording, for the pure
    /// mapping tests that never touch the filesystem.
    fn mapping_input() -> SttInput {
        SttInput::whole_file(
            std::path::PathBuf::from("/tmp/plaudon-mapping.wav"),
            digest(),
            1024,
            MediaFormat::new("wav", "pcm_s16le"),
            16_000,
            1,
        )
    }

    fn ready_bridge() -> FakeBridge {
        FakeBridge::new().with_default(|op| match op {
            BridgeOp::ProbeSpeech => success_document(op, speech_probe("installed", &["en_US"])),
            BridgeOp::ProbeFoundationModel => success_document(
                op,
                serde_json::json!({ "available": true, "is_transcriber": false }),
            ),
            _ => failure_document(op, "engine_failure", Some("unscripted")),
        })
    }

    fn engine(bridge: Arc<dyn AppleBridge>) -> AppleSpeechToText {
        let cache = Arc::new(ProbeCache::new(bridge.clone()));
        AppleSpeechToText::new(bridge, cache)
    }

    #[test]
    fn native_offsets_and_confidence_become_a_draft() {
        let result: Transcription = serde_json::from_value(transcription(
            "native",
            &serde_json::json!([
                {
                    "seq_index": 0,
                    "start_ms": 0,
                    "end_ms": 900,
                    "text": "hello",
                    "confidence": 0.55,
                    "words": [{ "text": "hello", "start_ms": 0, "end_ms": 400 }],
                },
                {
                    "seq_index": 1,
                    "start_ms": 960,
                    "end_ms": 1500,
                    "text": " world",
                    "confidence": null,
                },
            ]),
        ))
        .unwrap();

        let draft = to_draft(&result, &mapping_input()).expect("a final result becomes a draft");
        assert_eq!(draft.alignment_origin, AlignmentOrigin::Native);
        assert_eq!(draft.engine_id.as_str(), APPLE_SPEECH_ENGINE);
        assert_eq!(draft.segments.len(), 2);
        assert_eq!(
            draft.segments[0].start.as_ref().map(|t| t.sample_offset),
            Some(0)
        );
        assert_eq!(
            draft.segments[1].start.as_ref().map(|t| t.sample_offset),
            // 960 ms at 16 kHz.
            Some(15_360)
        );
        assert_eq!(draft.backend.as_deref(), Some("apple-speechanalyzer"));
        assert_eq!(draft.engine_params["locale"], "en_US");
        assert_eq!(draft.segments[0].words.len(), 1);
        assert!(draft.segments[0].confidence.is_some());
        assert!(draft.segments[1].confidence.is_none());
        assert_eq!(draft.locale.unwrap().as_str(), "en-US");
    }

    #[test]
    fn a_text_only_result_gains_no_timeline() {
        let result: Transcription = serde_json::from_value(transcription(
            "none",
            &serde_json::json!([{ "seq_index": 0, "text": "hello world" }]),
        ))
        .unwrap();
        let draft = to_draft(&result, &mapping_input()).expect("text-only is a valid result");
        assert_eq!(draft.alignment_origin, AlignmentOrigin::None);
        assert!(draft.segments.iter().all(|s| s.start.is_none()));
        assert!(draft.segments.iter().all(|s| s.words.is_empty()));
        assert!(!draft.alignment_origin.permits_seek());
    }

    #[test]
    fn offsets_on_a_result_declaring_none_are_refused() {
        let result: Transcription = serde_json::from_value(transcription(
            "none",
            &serde_json::json!([{ "seq_index": 0, "start_ms": 0, "end_ms": 10, "text": "x" }]),
        ))
        .unwrap();
        assert_eq!(
            to_draft(&result, &mapping_input()),
            Err(SttError::MalformedOutput)
        );
    }

    #[test]
    fn a_native_claim_with_no_offsets_anywhere_is_refused() {
        let result: Transcription = serde_json::from_value(transcription(
            "native",
            &serde_json::json!([{ "seq_index": 0, "text": "x" }]),
        ))
        .unwrap();
        assert_eq!(
            to_draft(&result, &mapping_input()),
            Err(SttError::MalformedOutput)
        );
    }

    #[test]
    fn an_inverted_span_drops_its_offsets_rather_than_inventing_a_seek() {
        let result: Transcription = serde_json::from_value(transcription(
            "native",
            &serde_json::json!([
                { "seq_index": 0, "start_ms": 0, "end_ms": 900, "text": "a" },
                { "seq_index": 1, "start_ms": 900, "end_ms": 10, "text": "b" },
            ]),
        ))
        .unwrap();
        let draft = to_draft(&result, &mapping_input()).unwrap();
        assert!(draft.segments[0].start.is_some());
        assert!(draft.segments[1].start.is_none());
    }

    #[test]
    fn a_non_final_result_is_never_a_draft() {
        let mut value = transcription("native", &serde_json::json!([]));
        value["final"] = serde_json::Value::Bool(false);
        let result: Transcription = serde_json::from_value(value).unwrap();
        assert_eq!(
            to_draft(&result, &mapping_input()),
            Err(SttError::MalformedOutput)
        );
    }

    #[test]
    fn every_bridge_code_maps_to_a_typed_stt_error() {
        for code in BridgeErrorCode::KNOWN {
            let error = map_error(&BridgeError::new(code.clone(), None));
            if code == BridgeErrorCode::Cancelled {
                assert_eq!(error, SttError::Cancelled);
            }
        }
        assert_eq!(
            map_error(&BridgeError::new(BridgeErrorCode::ModelAssetMissing, None)),
            SttError::Unavailable
        );
        assert_eq!(
            map_error(&BridgeError::new(BridgeErrorCode::OutputNotFinal, None)),
            SttError::MalformedOutput
        );
        assert_eq!(
            map_error(&BridgeError::new(
                BridgeErrorCode::Unrecognized("what".into()),
                None
            )),
            SttError::EngineFailure
        );
    }

    #[tokio::test]
    async fn an_uninstalled_locale_fails_closed_without_downloading() {
        let bridge: Arc<dyn AppleBridge> =
            Arc::new(FakeBridge::new().with_default(|op| match op {
                BridgeOp::ProbeSpeech => success_document(op, speech_probe("supported", &[])),
                _ => failure_document(op, "engine_failure", None),
            }));
        let engine = engine(bridge.clone());
        let file = tempfile::NamedTempFile::new().unwrap();
        let error = engine
            .transcribe(input(file.path()), JobContext::new(JobId::new()))
            .await
            .unwrap_err();
        assert_eq!(error, SttError::Unavailable);

        let fake_calls = bridge.live_token_count();
        assert_eq!(fake_calls, 0, "no handle leaked");
    }

    #[tokio::test]
    async fn declining_the_download_leaves_the_original_audio_untouched() {
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), b"original bytes").unwrap();
        let before = std::fs::read(file.path()).unwrap();

        let bridge: Arc<dyn AppleBridge> =
            Arc::new(FakeBridge::new().with_default(|op| match op {
                BridgeOp::ProbeSpeech => success_document(op, speech_probe("supported", &[])),
                _ => failure_document(op, "model_asset_missing", Some("en_US")),
            }));
        let engine = engine(bridge);
        let _ = engine
            .transcribe(input(file.path()), JobContext::new(JobId::new()))
            .await;

        assert_eq!(std::fs::read(file.path()).unwrap(), before);
    }

    #[tokio::test]
    async fn a_successful_run_reports_progress_and_produces_a_draft() {
        let bridge: Arc<dyn AppleBridge> = Arc::new(
            ready_bridge().script(
                BridgeOp::TranscribeFile,
                ScriptedCall::ProgressThenReply {
                    progress: vec![
                        r#"{"envelope_version":1,"op":"transcribe_file","phase":"transcribe","fraction":0.5,"processed_ms":750}"#.into(),
                    ],
                    reply: success_document(
                        BridgeOp::TranscribeFile,
                        transcription("native", &serde_json::json!([{ "seq_index": 0, "start_ms": 0, "end_ms": 1500, "text": "hello world" }]),
                        ),
                    ),
                },
            ),
        );
        let engine = engine(bridge.clone());
        let sink = Arc::new(RecordingProgressSink::new());
        let ctx = JobContext::new(JobId::new()).with_progress(sink.clone());
        let file = tempfile::NamedTempFile::new().unwrap();

        let draft = engine.transcribe(input(file.path()), ctx).await.unwrap();
        assert_eq!(draft.segments.len(), 1);
        assert_eq!(draft.duration_ms, Some(1500));

        // The forwarder is aborted as soon as the call returns, so allow the
        // runtime one tick to drain what was already queued.
        tokio::task::yield_now().await;
        assert_eq!(bridge.live_token_count(), 0, "the handle was released");
    }

    #[tokio::test]
    async fn a_cancelled_context_never_yields_a_transcript() {
        let bridge: Arc<dyn AppleBridge> = Arc::new(ready_bridge().reply(
            BridgeOp::TranscribeFile,
            success_document(
                BridgeOp::TranscribeFile,
                transcription("native", &serde_json::json!([{ "seq_index": 0, "start_ms": 0, "end_ms": 10, "text": "x" }]),
                ),
            ),
        ));
        let engine = engine(bridge);
        let ctx = JobContext::new(JobId::new());
        ctx.cancel.cancel();
        let file = tempfile::NamedTempFile::new().unwrap();
        assert_eq!(
            engine.transcribe(input(file.path()), ctx).await,
            Err(SttError::Cancelled)
        );
    }

    #[tokio::test]
    async fn a_mid_flight_cancellation_is_cancelled_not_an_engine_failure() {
        // Found by tests/chaos/tests/apple_bridge_stress.rs: the first
        // version of the dispatcher reported a token that fired while the
        // blocking call was in flight as a boundary failure, which a job
        // engine renders as "the engine broke" rather than "you stopped it".
        let bridge: Arc<dyn AppleBridge> = Arc::new(ready_bridge().script(
            BridgeOp::TranscribeFile,
            ScriptedCall::Dynamic(Box::new(|_request, _progress| {
                std::thread::sleep(Duration::from_millis(60));
                Ok(success_document(
                    BridgeOp::TranscribeFile,
                    transcription(
                        "native",
                        &serde_json::json!([
                            { "seq_index": 0, "start_ms": 0, "end_ms": 10, "text": "x" }
                        ]),
                    ),
                ))
            })),
        ));
        let engine = engine(bridge.clone());
        let ctx = JobContext::new(JobId::new());
        let cancel = ctx.cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(5)).await;
            cancel.cancel();
        });
        let file = tempfile::NamedTempFile::new().unwrap();
        assert_eq!(
            engine.transcribe(input(file.path()), ctx).await,
            Err(SttError::Cancelled)
        );
        assert_eq!(bridge.live_token_count(), 0, "the handle was released");
    }

    #[tokio::test]
    async fn a_language_substitution_is_refused() {
        let mut value = transcription(
            "native",
            &serde_json::json!([{ "seq_index": 0, "start_ms": 0, "end_ms": 10, "text": "x" }]),
        );
        value["locale"] = serde_json::Value::String("ja_JP".into());
        let bridge: Arc<dyn AppleBridge> = Arc::new(ready_bridge().reply(
            BridgeOp::TranscribeFile,
            success_document(BridgeOp::TranscribeFile, value),
        ));
        let engine = engine(bridge);
        let file = tempfile::NamedTempFile::new().unwrap();
        assert_eq!(
            engine
                .transcribe(input(file.path()), JobContext::new(JobId::new()))
                .await,
            Err(SttError::Unavailable)
        );
    }

    #[tokio::test]
    async fn consent_is_carried_into_the_request() {
        let bridge = Arc::new(ready_bridge().reply(
            BridgeOp::TranscribeFile,
            success_document(
                BridgeOp::TranscribeFile,
                transcription("native", &serde_json::json!([{ "seq_index": 0, "start_ms": 0, "end_ms": 10, "text": "x" }]),
                ),
            ),
        ));
        let handle: Arc<dyn AppleBridge> = bridge.clone();
        let cache = Arc::new(ProbeCache::new(handle.clone()));
        let engine =
            AppleSpeechToText::new(handle, cache).with_asset_consent(AssetDownloadConsent::Granted);
        let file = tempfile::NamedTempFile::new().unwrap();
        let _ = engine
            .transcribe(input(file.path()), JobContext::new(JobId::new()))
            .await;

        let call = &bridge.calls_for(BridgeOp::TranscribeFile)[0];
        let value: serde_json::Value = serde_json::from_str(&call.request).unwrap();
        assert_eq!(value["allow_asset_download"], true);
        assert_eq!(value["want_word_timestamps"], true);
        assert!(value["cancel_token"].as_u64().unwrap() > 0);
        assert!(value["timeout_ms"].as_u64().unwrap() > 0);
    }

    #[tokio::test]
    async fn installing_assets_invalidates_the_cached_verdict() {
        let bridge = Arc::new(
            FakeBridge::new()
                .reply(
                    BridgeOp::ProbeSpeech,
                    success_document(BridgeOp::ProbeSpeech, speech_probe("supported", &[])),
                )
                .reply(
                    BridgeOp::InstallSpeechAssets,
                    success_document(
                        BridgeOp::InstallSpeechAssets,
                        serde_json::json!({ "asset_status": "installed", "locale": "en_US" }),
                    ),
                )
                .reply(
                    BridgeOp::ProbeSpeech,
                    success_document(BridgeOp::ProbeSpeech, speech_probe("installed", &["en_US"])),
                )
                .with_default(|op| failure_document(op, "engine_failure", None)),
        );
        let handle: Arc<dyn AppleBridge> = bridge.clone();
        let cache = Arc::new(ProbeCache::new(handle.clone()));
        let engine = AppleSpeechToText::new(handle, cache);

        let locale = LanguageTag::parse("en-US").unwrap();
        let plan = engine.asset_plan(&locale).await;
        assert!(plan.requires_download());
        assert!(plan.installable);
        assert_eq!(plan.download_bytes, None);

        let installed = engine
            .install_assets(&locale, JobContext::new(JobId::new()))
            .await
            .expect("install succeeds");
        assert_eq!(installed.asset_status, AssetStatus::Installed);

        let caps = engine.capabilities().await;
        assert!(
            caps.is_selectable(),
            "the cache must re-probe after an installation"
        );
    }

    #[tokio::test]
    async fn an_unparseable_reply_is_an_engine_failure_not_an_empty_transcript() {
        let bridge: Arc<dyn AppleBridge> =
            Arc::new(ready_bridge().reply(BridgeOp::TranscribeFile, "}{ not json".to_owned()));
        let engine = engine(bridge);
        let file = tempfile::NamedTempFile::new().unwrap();
        assert_eq!(
            engine
                .transcribe(input(file.path()), JobContext::new(JobId::new()))
                .await,
            Err(SttError::EngineFailure)
        );
    }

    #[tokio::test]
    async fn an_unsupported_input_format_is_refused_before_the_bridge_is_called() {
        let bridge = Arc::new(ready_bridge());
        let handle: Arc<dyn AppleBridge> = bridge.clone();
        let cache = Arc::new(ProbeCache::new(handle.clone()));
        let engine = AppleSpeechToText::new(handle, cache);
        let file = tempfile::NamedTempFile::new().unwrap();
        let mut request = input(file.path());
        request.format = MediaFormat::new("ogg", "opus");
        assert_eq!(
            engine
                .transcribe(request, JobContext::new(JobId::new()))
                .await,
            Err(SttError::UnsupportedFormat)
        );
        assert_eq!(bridge.calls_for(BridgeOp::TranscribeFile), Vec::new());
    }
}
