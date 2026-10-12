//! Deterministic DTO fixtures for the AI panel tests.
//!
//! Every identifier and instant is fixed, so a frame rendered from these
//! fixtures is reproducible and may be snapshotted (`docs/EGUI_TESTING.md`
//! §4.5).

#![allow(dead_code)]

use plaudon_app::dto::{
    ActionItemFields, AnchorDto, Appearance, Capabilities, ClaimDto, CloudBudget, ConsentSummary,
    Contrast, CostConfidence, CostValue, CoverageDto, DecisionFields, EngineInventoryEntry,
    EngineStage, GetUsageResponse, HardwareInfo, InstalledModel, JobKind, JobPage, JobState,
    JobSummary, ListModelsResponse, ListOutputsResponse, ListSpeakersResponse, ModelCard,
    ModelFeatures, ModelPricing, ModelPrivacy, MoneyAmount, NotificationSettings, OsInfo,
    OutputDto, Platform, Preferences, RenderedClaimFields, SpeakerDto, StorageCapability, TriState,
    UsageTotals,
};
use plaudon_domain::capability::CapabilityStatus;
use plaudon_domain::error::{AppErrorCode, ErrorEnvelope};
use std::collections::BTreeMap;

pub const RECORDING_ID: &str = "11111111-1111-1111-1111-111111111111";
pub const REVISION_ID: &str = "22222222-2222-2222-2222-222222222222";
pub const OUTPUT_ID: &str = "33333333-3333-3333-3333-333333333333";
pub const CLAIM_ID: &str = "44444444-4444-4444-4444-444444444444";
pub const SECOND_CLAIM_ID: &str = "55555555-5555-5555-5555-555555555555";
pub const JOB_ID: &str = "66666666-6666-6666-6666-666666666666";
pub const SPEAKER_ID: &str = "77777777-7777-7777-7777-777777777777";

/// A fixed wall clock for elapsed labels.
pub const NOW_MS: i128 = 1_770_000_000_000;

pub fn usd(amount: &str) -> MoneyAmount {
    MoneyAmount {
        amount: amount.to_owned(),
        currency: "USD".to_owned(),
    }
}

pub fn local_model() -> ModelCard {
    ModelCard {
        model_id: "whisper-small".to_owned(),
        display_name: "Whisper small".to_owned(),
        engine: "whisper.cpp".to_owned(),
        request_family: "multipart_stt".to_owned(),
        verification: "verified".to_owned(),
        verification_reason: None,
        fixture_id: Some("fixture-1".to_owned()),
        validated_at: Some("2026-01-01T00:00:00Z".to_owned()),
        features: ModelFeatures {
            segments: TriState::Yes,
            word_timestamps: TriState::No,
            diarization: TriState::Unknown,
            language_detection: TriState::Yes,
        },
        locales: Some(vec!["en".to_owned()]),
        max_input_bytes: None,
        max_duration_ms: None,
        pricing: ModelPricing {
            basis: "none".to_owned(),
            unit_label: "runs on this computer".to_owned(),
            amount_per_unit: None,
        },
        privacy: ModelPrivacy {
            relay: "whisper.cpp".to_owned(),
            downstream_provider: None,
            retention: "zero_retention".to_owned(),
            retention_days: None,
            zero_retention_available: true,
        },
        selectable: true,
    }
}

pub fn cloud_model() -> ModelCard {
    ModelCard {
        model_id: "vendor/whisper-large".to_owned(),
        display_name: "Vendor Whisper Large".to_owned(),
        engine: "openrouter".to_owned(),
        request_family: "multipart_stt".to_owned(),
        verification: "verified".to_owned(),
        verification_reason: None,
        fixture_id: Some("fixture-2".to_owned()),
        validated_at: Some("2026-01-02T00:00:00Z".to_owned()),
        features: ModelFeatures {
            segments: TriState::Yes,
            word_timestamps: TriState::Yes,
            diarization: TriState::No,
            language_detection: TriState::Yes,
        },
        locales: None,
        max_input_bytes: None,
        max_duration_ms: None,
        pricing: ModelPricing {
            basis: "per_audio_second".to_owned(),
            unit_label: "per audio second".to_owned(),
            amount_per_unit: Some(usd("0.000100")),
        },
        privacy: ModelPrivacy {
            relay: "openrouter".to_owned(),
            downstream_provider: Some("vendor".to_owned()),
            retention: "provider_default".to_owned(),
            retention_days: None,
            zero_retention_available: false,
        },
        selectable: true,
    }
}

pub fn models(cards: Vec<ModelCard>) -> ListModelsResponse {
    ListModelsResponse {
        default_model_id: cards.first().map(|card| card.model_id.clone()),
        models: cards,
        catalog_source: "live".to_owned(),
        catalog_fetched_at: Some("2026-02-01T00:00:00Z".to_owned()),
        degraded_reason: None,
    }
}

pub fn engine(id: &str, name: &str, requires_network: bool) -> EngineInventoryEntry {
    EngineInventoryEntry {
        engine_id: id.to_owned(),
        display_name: name.to_owned(),
        stage: EngineStage::Stt,
        status: CapabilityStatus::Supported,
        unavailable_reason: None,
        installed_models: vec![InstalledModel {
            model_id: "whisper-small".to_owned(),
            display_name: "Whisper small".to_owned(),
            size_bytes: 480_000_000,
            installed: !requires_network,
        }],
        download_bytes_required: None,
        requires_network,
    }
}

pub fn capabilities(local_only: bool, consents: Vec<ConsentSummary>) -> Capabilities {
    Capabilities {
        os: OsInfo {
            platform: Platform::Macos,
            version: "26.0".to_owned(),
        },
        hardware: HardwareInfo {
            arch: "aarch64".to_owned(),
            logical_cores: 10,
            total_memory_bytes: 32_000_000_000,
        },
        engines: vec![
            engine("apple.speech", "Apple native speech", false),
            engine("whisper.cpp", "whisper.cpp", false),
            engine("openrouter", "OpenRouter", true),
        ],
        storage: StorageCapability {
            library_path: "/library".to_owned(),
            free_bytes: 100_000_000_000,
            total_bytes: 500_000_000_000,
        },
        integrations: BTreeMap::new(),
        permissions: BTreeMap::new(),
        preferences: Preferences {
            onboarding_completed: true,
            local_only_mode: local_only,
            default_stt_engine_id: Some("whisper.cpp".to_owned()),
            default_text_engine_id: None,
            appearance: Appearance::Light,
            contrast: Contrast::Standard,
            notifications: NotificationSettings {
                enabled: true,
                categories: BTreeMap::new(),
            },
        },
        openrouter_key_present: true,
        cloud_consents: consents,
    }
}

pub fn grant(scope_kind: &str, model_id: &str, recording_id: Option<&str>) -> ConsentSummary {
    ConsentSummary {
        consent_id: "consent-1".to_owned(),
        purpose: "transcription".to_owned(),
        content_type: "audio_bytes".to_owned(),
        scope_kind: scope_kind.to_owned(),
        recording_id: recording_id.map(str::to_owned),
        model_id: model_id.to_owned(),
        downstream_provider: Some("vendor".to_owned()),
        required_retention: None,
        disclosure_version: "2026-02-01".to_owned(),
        granted_at: "2026-02-01T00:00:00Z".to_owned(),
        expires_at: None,
        revoked_at: None,
    }
}

pub fn job(state: JobState, progress: Option<f64>) -> JobSummary {
    JobSummary {
        job_id: JOB_ID.parse().expect("job id"),
        kind: JobKind::Transcribe,
        state,
        stage: Some("transcribe".to_owned()),
        progress,
        created_at: "2026-02-01T10:00:00Z".to_owned(),
        updated_at: "2026-02-01T10:00:10Z".to_owned(),
        started_at: Some("2026-02-01T10:00:00Z".to_owned()),
        finished_at: None,
        recording_id: Some(RECORDING_ID.parse().expect("recording id")),
        label: "Meeting with the board".to_owned(),
        error: None,
        cost_acknowledgement_required: false,
        provider_request_id: None,
        output_link: None,
        cost: CostValue::unknown(),
    }
}

pub fn envelope(code: AppErrorCode) -> ErrorEnvelope {
    ErrorEnvelope {
        code,
        message: code.canonical_message().to_owned(),
        retryable: code.is_retryable(),
        action: code.action(),
        diagnostic_id: "88888888-8888-8888-8888-888888888888"
            .parse()
            .expect("diagnostic id"),
    }
}

pub fn job_page(items: Vec<JobSummary>) -> JobPage {
    JobPage {
        items,
        next_cursor: None,
        state_counts: BTreeMap::new(),
    }
}

pub fn anchor(verified: bool, seek_ms: Option<u64>) -> AnchorDto {
    AnchorDto {
        seq_index: 7,
        char_start: 0,
        char_end: 24,
        excerpt: "we will ship on Friday".to_owned(),
        verified,
        rejection_code: if verified {
            None
        } else {
            Some("no_match_in_revision".to_owned())
        },
        matched_text: verified.then(|| "we will ship on Friday".to_owned()),
        alignment_origin: if seek_ms.is_some() { "native" } else { "none" }.to_owned(),
        seek_ms,
    }
}

pub fn action_item_claim(state: &str, review_required: bool, anchors: Vec<AnchorDto>) -> ClaimDto {
    ClaimDto {
        claim_id: CLAIM_ID.parse().expect("claim id"),
        item_path: "/action_items/0".to_owned(),
        text: "Ship the release".to_owned(),
        state: state.to_owned(),
        review_required,
        review_reasons: if review_required {
            vec!["anchor_unresolved".to_owned()]
        } else {
            Vec::new()
        },
        entailment: "not_checked".to_owned(),
        category: "obligation".to_owned(),
        fields: RenderedClaimFields::ActionItem(ActionItemFields {
            verb: "Ship".to_owned(),
            description: "Ship the release".to_owned(),
            owner: "Unassigned".to_owned(),
            due_date: "Not specified".to_owned(),
            status: "open".to_owned(),
            assertion: "directly_stated".to_owned(),
        }),
        anchors,
    }
}

pub fn decision_claim(state: &str) -> ClaimDto {
    ClaimDto {
        claim_id: SECOND_CLAIM_ID.parse().expect("claim id"),
        item_path: "/decisions/0".to_owned(),
        text: "The board approved the budget".to_owned(),
        state: state.to_owned(),
        review_required: false,
        review_reasons: Vec::new(),
        entailment: "supported".to_owned(),
        category: "general".to_owned(),
        fields: RenderedClaimFields::Decision(DecisionFields {
            statement: "The board approved the budget".to_owned(),
            participants: Vec::new(),
            time_reference: "Not specified".to_owned(),
            assertion: "directly_stated".to_owned(),
        }),
        anchors: vec![anchor(true, Some(12_000))],
    }
}

pub fn output(claims: Vec<ClaimDto>, stale: Option<(&str, &str)>) -> OutputDto {
    OutputDto {
        output_id: OUTPUT_ID.parse().expect("output id"),
        recording_id: RECORDING_ID.parse().expect("recording id"),
        transcript_revision_id: REVISION_ID.parse().expect("revision id"),
        output_type: "meeting_summary".to_owned(),
        template_id: "meeting.v1".to_owned(),
        template_version: "1.2.0".to_owned(),
        pipeline_version: "3".to_owned(),
        status: if claims.iter().any(|claim| claim.review_required) {
            "needs_review".to_owned()
        } else {
            "complete".to_owned()
        },
        coverage: CoverageDto {
            state: "full".to_owned(),
            chunks: None,
        },
        stale: stale.map(|(at, reason)| plaudon_app::dto::StaleDto {
            stale_at: at.to_owned(),
            reason: reason.to_owned(),
        }),
        created_at: "2026-02-01T11:00:00Z".to_owned(),
        claims,
    }
}

pub fn outputs(outputs: Vec<OutputDto>) -> ListOutputsResponse {
    ListOutputsResponse { outputs }
}

pub fn speakers() -> ListSpeakersResponse {
    ListSpeakersResponse {
        speakers: vec![SpeakerDto {
            speaker_id: SPEAKER_ID.parse().expect("speaker id"),
            recording_id: RECORDING_ID.parse().expect("recording id"),
            label: "Speaker 1".to_owned(),
            confirmed: false,
            original_label: "SPEAKER_00".to_owned(),
            renamed_at: None,
            confirmed_at: None,
        }],
    }
}

pub fn usage(unknown_count: u32) -> GetUsageResponse {
    GetUsageResponse {
        from: "2026-02-01T00:00:00Z".to_owned(),
        to: "2026-03-01T00:00:00Z".to_owned(),
        totals: UsageTotals {
            finalized: Some(usd("1.250000")),
            estimated: Some(usd("0.400000")),
            unknown_count,
        },
        by_model: Vec::new(),
        attempts: Vec::new(),
        budget: CloudBudget {
            per_job_limit: Some(usd("0.500000")),
            monthly_limit: None,
            month_to_date: CostValue {
                confidence: CostConfidence::Estimated,
                amount: Some(usd("1.650000")),
            },
            blocks_dispatch: true,
            enforcement_note: "A local cap cannot prevent a charge a provider has already \
                               accepted."
                .to_owned(),
        },
    }
}
