//! Contract tests driving every capability trait through its in-memory fake
//! (spec 6.3(9), 18.3). No vendor crate, network or paid endpoint is involved.

use std::sync::Arc;

use plaudon_ai_contracts::capabilities::{
    AvailabilityReason, MediaFormat, PrivacyClass, RequestFamily, SttCapabilities,
    TextModelCapabilities,
};
use plaudon_ai_contracts::job::{JobContext, RecordingProgressSink};
use plaudon_ai_contracts::policy::{select_stt_engine, EngineRejection, SttRequirements};
use plaudon_ai_contracts::stt::{SpeechToText, SttError, SttInput, TranscriptSegmentDraft};
use plaudon_ai_contracts::text::{StructuredRequest, TextModel, TextModelError};
use plaudon_ai_contracts::transcript::{ChunkingStrategy, SourceTime};
use plaudon_domain::capability::{CapabilityStatus, Known, Tri};
use plaudon_domain::ids::{DeviceRecordingId, JobId, RecordingId};
use plaudon_domain::locale::LanguageTag;
use plaudon_domain::media::{MediaIdentity, QualityClass, Sha256Digest};
use plaudon_domain::model::{EngineId, ModelIdentity};
use plaudon_domain::source::{
    ExternalRecordingId, SourceAccountScope, SourceIdentityKey, SourceKind,
};
use plaudon_domain::time::Timestamp;
use plaudon_domain::transcript::{
    AlignmentOrigin, RevisionOrigin, RevisionStatus, TranscriptRevision, TranscriptRevisionDraft,
};
use plaudon_integration_tests::fakes::device::FakeDeviceRecording;
use plaudon_integration_tests::fakes::source::FakeSourceItem;
use plaudon_integration_tests::fakes::{FakeDevice, FakeSource, FakeSpeechToText, FakeTextModel};
use plaudon_recording_sources::device::{
    AuthScheme, DeviceCapabilities, DeviceError, DeviceRecording, DirectDevice, Transport,
};
use plaudon_recording_sources::sink::InMemorySink;
use plaudon_recording_sources::source::{
    RecordingSource, SourceCapabilities, SourceError, SourceItem,
};
use tokio_util::sync::CancellationToken;

fn source_item(external: &str, name: &str) -> SourceItem {
    SourceItem {
        identity: SourceIdentityKey::new(
            SourceKind::PlaudCloudCli,
            SourceAccountScope::new("account-a"),
            Some(ExternalRecordingId::new(external)),
        ),
        display_name: name.into(),
        captured_at: Some(Timestamp::from_millis(1_000)),
        duration_ms: Some(60_000),
        declared_byte_length: Some(5),
        remote_version: Some("v1".into()),
        quality_class: QualityClass::VendorCloudExport,
    }
}

fn listing_source() -> SourceCapabilities {
    let mut caps = SourceCapabilities::unknown(SourceKind::PlaudCloudCli);
    caps.status = CapabilityStatus::Supported;
    caps.supports_listing = Tri::Yes;
    caps.supports_pagination = Tri::Yes;
    caps.supports_stable_external_ids = Tri::Yes;
    caps
}

fn local_stt() -> SttCapabilities {
    let engine = EngineId::new("fake-local");
    let mut caps = SttCapabilities::unknown(engine.clone(), ModelIdentity::new(engine, "tiny"));
    caps.status = CapabilityStatus::Supported;
    caps.availability_reason = AvailabilityReason::Available;
    caps.offline = Tri::Yes;
    caps.can_cancel = Tri::Yes;
    caps.supports_segments = Tri::Yes;
    caps.privacy_class = Known::Known(PrivacyClass::OnDevice);
    caps.supported_formats = Known::Known(vec![MediaFormat::new("wav", "pcm_s16le")]);
    caps.locales = Known::Known(vec![LanguageTag::parse("en").unwrap()]);
    caps
}

fn stt_input(bytes: &[u8]) -> SttInput {
    SttInput {
        media_path: std::path::PathBuf::from("/does/not/need/to/exist.wav"),
        media_sha256: Sha256Digest::of(bytes),
        byte_length: bytes.len() as u64,
        format: MediaFormat::new("wav", "pcm_s16le"),
        duration_ms: Some(1_000),
        locale_hint: Some(LanguageTag::parse("en-US").unwrap()),
        sample_rate_hz: 16_000,
        channels: 1,
        source_sample_offset: 0,
        normalization_profile: None,
        chunking_strategy: ChunkingStrategy::SingleChunk,
    }
}

fn at(millis: u64) -> SourceTime {
    SourceTime::from_millis(millis, 16_000)
}

fn validated_device_cell() -> DeviceCapabilities {
    let mut caps = DeviceCapabilities::unknown("PLAUD", "Note", "macOS 26.0");
    caps.status = CapabilityStatus::Supported;
    caps.auth_scheme = AuthScheme::LegacyStaticToken;
    caps.supports_read_only_listing = Tri::Yes;
    caps.observed_firmware = Known::Known("V0095".into());
    caps.validated_at = Known::Known(Timestamp::from_millis(1));
    caps.test_run_id = Known::Known("run-ble-001".into());
    caps.transport = Transport::Ble;
    caps.quality_class = QualityClass::CompressedDeviceTransfer;
    caps
}

#[tokio::test]
async fn a_fake_source_paginates_and_acquires_without_touching_the_network() {
    let staging = tempfile::tempdir().unwrap();
    let source = FakeSource::new(
        listing_source(),
        vec![
            vec![FakeSourceItem {
                item: source_item("rec-1", "Monday standup"),
                bytes: b"audio".to_vec(),
            }],
            vec![FakeSourceItem {
                item: source_item("rec-2", "Tuesday review"),
                bytes: b"other".to_vec(),
            }],
        ],
    );

    let first = source.list(None).await.unwrap();
    assert_eq!(first.items.len(), 1);
    let cursor = first
        .next_cursor
        .clone()
        .expect("a second page must be advertised");
    let second = source.list(Some(&cursor)).await.unwrap();
    assert_eq!(second.items[0].display_name, "Tuesday review");
    assert_eq!(second.next_cursor, None);

    let target = staging.path().join("rec-1.part");
    let acquired = source.acquire(&first.items[0], &target).await.unwrap();
    assert_eq!(
        acquired.identity,
        MediaIdentity::of_bytes(b"audio", "pcm_s16le")
    );
    assert_eq!(std::fs::read(&target).unwrap(), b"audio");
    assert_eq!(source.acquisitions(), ["Monday standup"]);
}

#[tokio::test]
async fn source_failures_surface_as_stable_errors() {
    let source = FakeSource::new(listing_source(), Vec::new());
    source.fail_next(SourceError::AuthenticationRequired);
    assert_eq!(
        source.list(None).await.unwrap_err(),
        SourceError::AuthenticationRequired
    );

    let missing = source_item("absent", "Missing");
    let staging = tempfile::tempdir().unwrap();
    assert_eq!(
        source
            .acquire(&missing, &staging.path().join("x.part"))
            .await
            .unwrap_err(),
        SourceError::AssetNotFound
    );
}

#[tokio::test]
async fn a_device_streams_into_staging_and_reports_completeness() {
    let listing = DeviceRecording {
        id: DeviceRecordingId::new(),
        display_name: "REC0001.ASR".into(),
        declared_byte_length: Some(6),
        captured_at: None,
        declared_codec: Some("opus".into()),
    };
    let id = listing.id;
    let device = FakeDevice::new(
        Ok(validated_device_cell()),
        vec![FakeDeviceRecording {
            listing,
            chunks: vec![b"abc".to_vec(), b"def".to_vec()],
            emits_terminal_marker: true,
        }],
    );

    assert_eq!(device.list_available().await.unwrap().len(), 1);
    let mut sink = InMemorySink::with_budget(1_024);
    let transfer = device
        .read_to_staging(&id, &mut sink, &CancellationToken::new())
        .await
        .unwrap();

    assert_eq!(sink.bytes(), b"abcdef");
    assert!(transfer.is_complete());
    assert_eq!(
        transfer.quality_class,
        QualityClass::CompressedDeviceTransfer
    );
    assert!(!transfer.quality_class.is_original_fidelity());
}

#[tokio::test]
async fn a_transfer_without_a_terminal_marker_is_not_complete() {
    let listing = DeviceRecording {
        id: DeviceRecordingId::new(),
        display_name: "REC0002.ASR".into(),
        declared_byte_length: Some(3),
        captured_at: None,
        declared_codec: Some("opus".into()),
    };
    let id = listing.id;
    let device = FakeDevice::new(
        Ok(validated_device_cell()),
        vec![FakeDeviceRecording {
            listing,
            chunks: vec![b"abc".to_vec()],
            emits_terminal_marker: false,
        }],
    );
    let mut sink = InMemorySink::with_budget(1_024);
    let transfer = device
        .read_to_staging(&id, &mut sink, &CancellationToken::new())
        .await
        .unwrap();
    assert!(!transfer.is_complete());
}

#[tokio::test]
async fn a_cancelled_transfer_yields_a_typed_error_and_no_verified_transfer() {
    let listing = DeviceRecording {
        id: DeviceRecordingId::new(),
        display_name: "REC0003.ASR".into(),
        declared_byte_length: Some(6),
        captured_at: None,
        declared_codec: Some("opus".into()),
    };
    let id = listing.id;
    let device = FakeDevice::new(
        Ok(validated_device_cell()),
        vec![FakeDeviceRecording {
            listing,
            chunks: vec![b"abc".to_vec(), b"def".to_vec()],
            emits_terminal_marker: true,
        }],
    );

    let cancel = CancellationToken::new();
    cancel.cancel();
    let mut sink = InMemorySink::with_budget(1_024);
    let result = device.read_to_staging(&id, &mut sink, &cancel).await;
    assert_eq!(result.unwrap_err(), DeviceError::Cancelled);
    assert!(
        sink.bytes().is_empty(),
        "a cancelled transfer must not leave staged bytes"
    );
}

#[tokio::test]
async fn an_unvalidated_device_cell_refuses_to_list_or_read() {
    let mut caps = validated_device_cell();
    caps.observed_firmware = Known::Unknown;
    let device = FakeDevice::new(Ok(caps), Vec::new());
    assert_eq!(
        device.list_available().await.unwrap_err(),
        DeviceError::UnsupportedFirmware
    );
}

#[tokio::test]
async fn an_interrupted_transfer_reports_disconnection_rather_than_partial_success() {
    let listing = DeviceRecording {
        id: DeviceRecordingId::new(),
        display_name: "REC0004.ASR".into(),
        declared_byte_length: Some(6),
        captured_at: None,
        declared_codec: Some("opus".into()),
    };
    let id = listing.id;
    let device = FakeDevice::new(
        Ok(validated_device_cell()),
        vec![FakeDeviceRecording {
            listing,
            chunks: vec![b"abc".to_vec(), b"def".to_vec()],
            emits_terminal_marker: true,
        }],
    );
    device.disconnect_after(1);

    let mut sink = InMemorySink::with_budget(1_024);
    let result = device
        .read_to_staging(&id, &mut sink, &CancellationToken::new())
        .await;
    assert_eq!(result.unwrap_err(), DeviceError::DeviceDisconnected);
    assert_eq!(
        sink.bytes(),
        b"abc",
        "partial bytes stay in staging, never in the archive"
    );
}

#[tokio::test]
async fn a_fake_engine_drives_the_transcription_use_case_end_to_end() {
    let sink = Arc::new(RecordingProgressSink::new());
    let engine = FakeSpeechToText::new(
        local_stt(),
        AlignmentOrigin::Native,
        vec![
            TranscriptSegmentDraft {
                seq_index: 0,
                start: Some(at(0)),
                end: Some(at(500)),
                text: "hello".into(),
                confidence: None,
                speaker_id: None,
                words: Vec::new(),
            },
            TranscriptSegmentDraft {
                seq_index: 1,
                start: Some(at(500)),
                end: Some(at(1_000)),
                text: "world".into(),
                confidence: None,
                speaker_id: None,
                words: Vec::new(),
            },
        ],
    );

    let ctx = JobContext::new(JobId::new()).with_progress(sink.clone());
    let draft = engine.transcribe(stt_input(b"audio"), ctx).await.unwrap();
    assert_eq!(draft.alignment_origin, AlignmentOrigin::Native);
    assert_eq!(sink.updates().len(), 2);

    let revision = TranscriptRevision::try_new(TranscriptRevisionDraft {
        recording_id: RecordingId::new(),
        parent_id: None,
        origin: RevisionOrigin::Machine,
        input_sha256: draft.input_sha256,
        engine: draft.engine_id.clone(),
        model: draft.model.clone(),
        locale: draft.locale.clone(),
        alignment_origin: draft.alignment_origin,
        status: RevisionStatus::Complete,
        normalization_profile: "nfc-basic".into(),
        chunking_strategy: "single-pass".into(),
        duration_ms: draft.duration_ms,
        created_at: Timestamp::from_millis(10),
        segments: draft
            .segments
            .iter()
            .map(TranscriptSegmentDraft::to_domain_segment)
            .collect(),
    })
    .unwrap();

    assert!(revision.permits_seek());
    assert_eq!(revision.seek_offsets().unwrap().len(), 2);
    assert_eq!(revision.input_sha256, Sha256Digest::of(b"audio"));
}

#[tokio::test]
async fn a_cancelled_transcription_returns_cancelled_not_a_partial_draft() {
    let engine = FakeSpeechToText::new(
        local_stt(),
        AlignmentOrigin::Native,
        vec![TranscriptSegmentDraft {
            seq_index: 0,
            start: Some(at(0)),
            end: Some(at(10)),
            text: "x".into(),
            confidence: None,
            speaker_id: None,
            words: Vec::new(),
        }],
    );
    let ctx = JobContext::new(JobId::new());
    ctx.cancel.cancel();
    assert_eq!(
        engine
            .transcribe(stt_input(b"audio"), ctx)
            .await
            .unwrap_err(),
        SttError::Cancelled
    );
}

#[tokio::test]
async fn engine_policy_refuses_an_engine_that_is_not_supported() {
    let mut experimental = local_stt();
    experimental.status = CapabilityStatus::Experimental;
    let engine = FakeSpeechToText::new(experimental, AlignmentOrigin::None, Vec::new());

    let caps = engine.capabilities().await;
    let selection = select_stt_engine(
        std::slice::from_ref(&caps),
        &SttRequirements {
            format: Some(MediaFormat::new("wav", "pcm_s16le")),
            require_offline: true,
            ..SttRequirements::default()
        },
    );
    assert_eq!(selection.chosen, None);
    assert_eq!(selection.rejected[0].1, EngineRejection::NotSupportedStatus);
}

#[tokio::test]
async fn a_cloud_model_with_an_unknown_request_family_is_never_dispatched() {
    let mut cloud = local_stt();
    cloud.engine_id = EngineId::new("fake-cloud");
    cloud.offline = Tri::No;
    cloud.privacy_class = Known::Known(PrivacyClass::CloudZeroRetention);
    cloud.request_family = RequestFamily::Unknown;

    let selection = select_stt_engine(
        &[cloud],
        &SttRequirements {
            format: Some(MediaFormat::new("wav", "pcm_s16le")),
            ..SttRequirements::default()
        },
    );
    assert_eq!(selection.chosen, None);
    assert_eq!(selection.rejected[0].1, EngineRejection::NoRequestBuilder);
}

#[tokio::test]
async fn a_text_model_returns_a_validated_body_and_maps_refusals_to_errors() {
    let engine = EngineId::new("fake-text");
    let mut caps = TextModelCapabilities::unknown(engine.clone(), ModelIdentity::new(engine, "m"));
    caps.status = CapabilityStatus::Supported;
    caps.availability_reason = AvailabilityReason::Available;
    caps.supports_structured_output = Tri::Yes;

    let request = StructuredRequest {
        prompt_version: "v1".into(),
        prompt: "summarize".into(),
        schema_version: "v1".into(),
        output_schema: serde_json::json!({"type": "object"}),
        max_output_tokens: Some(128),
    };

    let model = FakeTextModel::new(caps.clone(), serde_json::json!({"summary": "ok"}));
    let response = model
        .generate_structured(request.clone(), JobContext::new(JobId::new()))
        .await
        .unwrap();
    assert_eq!(response.body["summary"], "ok");
    assert_eq!(response.repair_attempts, 0);

    let refusing = FakeTextModel::failing(caps, TextModelError::ProviderRejected);
    assert_eq!(
        refusing
            .generate_structured(request, JobContext::new(JobId::new()))
            .await
            .unwrap_err(),
        TextModelError::ProviderRejected
    );
}
