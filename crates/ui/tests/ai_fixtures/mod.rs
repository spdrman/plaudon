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
        catalogue: Vec::new(),
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
            export_defaults: plaudon_app::dto::ExportDefaults::default(),
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

/// One speaker row, as `list_speakers` and the three labelling commands
/// describe it. `name` is what a user typed; `confirmed` is their claim.
pub fn speaker(name: Option<&str>, confirmed: bool) -> SpeakerDto {
    SpeakerDto {
        speaker_id: SPEAKER_ID.parse().expect("speaker id"),
        recording_id: RECORDING_ID.parse().expect("recording id"),
        // The cluster this row stands for, as a diarized recording has.
        cluster_key: Some("SPEAKER_00".to_owned()),
        // No roster person: these fixtures are a recording nobody has
        // assigned yet.
        person_id: None,
        // A rename overwrites the label, exactly as the store does.
        label: name.unwrap_or("Speaker 1").to_owned(),
        name: name.map(ToOwned::to_owned),
        confirmed,
        original_label: "SPEAKER_00".to_owned(),
        renamed_at: name.map(|_| plaudon_domain::time::Timestamp::from_millis(1_770_000_000_000)),
        confirmed_at: confirmed
            .then(|| plaudon_domain::time::Timestamp::from_millis(1_770_000_060_000)),
    }
}

pub fn speakers() -> ListSpeakersResponse {
    ListSpeakersResponse {
        speakers: vec![speaker(None, false)],
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

/* ------------------------------------------------- transcript tools (A11) */

use plaudon_app::dto::{
    AskRecordingResponse, ConsentPolicyDto, ConversationDto, ConversationTurnDto, DefinitionDto,
    FlashcardDto, GenerateStudyArtifactsResponse, ListConversationsResponse,
    ListStudyArtifactsResponse, ListToolRunsResponse, ListTranscriptToolsResponse, QuizItemDto,
    RejectedAnchorDto, RejectedStudyItemDto, StudyArtifactSetDto, StudyArtifactsDto, TextRouteDto,
    ToolRunDto, TranscriptToolDto,
};

pub const CONVERSATION_ID: &str = "99999999-9999-9999-9999-999999999999";
pub const TOOL_RUN_ID: &str = "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa";

/// A text route; `model_id` is `None` for a route that names no model.
pub fn text_route(engine: &str, model_id: Option<&str>, is_cloud: bool) -> TextRouteDto {
    TextRouteDto {
        engine: engine.to_owned(),
        model_id: model_id.map(str::to_owned),
        is_cloud,
        available: true,
        unavailable_reason: None,
    }
}

/// The route of `local_model()`: an engine the inventory reports as
/// network-free.
pub fn local_text_route() -> TextRouteDto {
    text_route("whisper.cpp", Some("whisper-small"), false)
}

/// The route of `cloud_model()`: an engine the inventory reports as requiring
/// the network.
pub fn cloud_text_route() -> TextRouteDto {
    text_route("openrouter", Some("vendor/whisper-large"), true)
}

pub fn transcript_tool(
    tool_id: &str,
    kind: &str,
    display_name: &str,
    offerable: bool,
    unavailable_reason: Option<&str>,
) -> TranscriptToolDto {
    TranscriptToolDto {
        tool_id: tool_id.to_owned(),
        version: 1,
        kind: kind.to_owned(),
        display_name: display_name.to_owned(),
        instruction: "Produce study artifacts from the provided passages.".to_owned(),
        prompt_version: format!("{tool_id}/1"),
        output_schema_version: format!("{tool_id}.v1"),
        origin: "built_in".to_owned(),
        optional: true,
        offerable,
        unavailable_reason: unavailable_reason.map(str::to_owned),
    }
}

pub fn transcript_tools(
    tools: Vec<TranscriptToolDto>,
    text_model: Option<TextRouteDto>,
) -> ListTranscriptToolsResponse {
    ListTranscriptToolsResponse { tools, text_model }
}

/// The two study tools, one offerable and one refused by the model's
/// capabilities.
pub fn study_tools(route: TextRouteDto) -> ListTranscriptToolsResponse {
    transcript_tools(
        vec![
            transcript_tool("flashcards", "flashcards", "Flashcards", true, None),
            transcript_tool(
                "quiz",
                "quiz",
                "Quiz",
                false,
                Some("This model does not guarantee schema-constrained output."),
            ),
            transcript_tool(
                "meeting_summary",
                "meeting_summary",
                "Meeting summary",
                true,
                None,
            ),
        ],
        Some(route),
    )
}

pub fn rejected_anchor(rejection_code: &str) -> RejectedAnchorDto {
    RejectedAnchorDto {
        seq_index: 12,
        char_start: 0,
        char_end: 18,
        excerpt: "the budget tripled".to_owned(),
        rejection_code: rejection_code.to_owned(),
    }
}

pub fn ask_answer(
    outcome: &str,
    text: &str,
    anchors: Vec<AnchorDto>,
    rejected_anchors: Vec<RejectedAnchorDto>,
    dispatched: bool,
) -> AskRecordingResponse {
    AskRecordingResponse {
        conversation_id: CONVERSATION_ID.to_owned(),
        transcript_revision_id: REVISION_ID.to_owned(),
        outcome: outcome.to_owned(),
        text: text.to_owned(),
        anchors,
        rejected_anchors,
        retrieved_segments: vec![7, 8],
        dispatched,
        tool_run_id: dispatched.then(|| TOOL_RUN_ID.to_owned()),
        tool_id: "ask_recording".to_owned(),
        tool_version: 1,
    }
}

pub fn turn(
    seq_index: u32,
    role: &str,
    text: &str,
    outcome: Option<&str>,
    anchors: Vec<AnchorDto>,
) -> ConversationTurnDto {
    ConversationTurnDto {
        message_id: format!("message-{seq_index}"),
        seq_index,
        role: role.to_owned(),
        text: text.to_owned(),
        outcome: outcome.map(str::to_owned),
        anchors,
        tool_run_id: (role == "assistant").then(|| TOOL_RUN_ID.to_owned()),
        created_at: "2026-02-01T12:00:00Z".to_owned(),
    }
}

pub fn conversations(
    revision_is_current: bool,
    turns: Vec<ConversationTurnDto>,
) -> ListConversationsResponse {
    ListConversationsResponse {
        conversations: vec![ConversationDto {
            conversation_id: CONVERSATION_ID.to_owned(),
            recording_id: RECORDING_ID.to_owned(),
            transcript_revision_id: REVISION_ID.to_owned(),
            revision_is_current,
            created_at: "2026-02-01T12:00:00Z".to_owned(),
            turns,
        }],
    }
}

/// One of each study artifact kind, each with one resolved citation.
pub fn study_artifacts() -> StudyArtifactsDto {
    StudyArtifactsDto {
        flashcards: vec![FlashcardDto {
            front: "When does the release ship?".to_owned(),
            back: "On Friday.".to_owned(),
            anchors: vec![anchor(true, Some(90_000))],
        }],
        quiz: vec![QuizItemDto {
            question: "Which day was agreed for the release?".to_owned(),
            options: vec!["Thursday".to_owned(), "Friday".to_owned()],
            answer_index: 1,
            anchors: vec![anchor(true, None)],
        }],
        definitions: vec![DefinitionDto {
            term: "Release window".to_owned(),
            definition: "The period in which the build may be shipped.".to_owned(),
            anchors: vec![anchor(true, None)],
        }],
    }
}

pub fn rejected_study_item(path: &str, reason: &str, detail: &str) -> RejectedStudyItemDto {
    RejectedStudyItemDto {
        path: path.to_owned(),
        reason: reason.to_owned(),
        detail: detail.to_owned(),
    }
}

pub fn study_run(
    artifacts: StudyArtifactsDto,
    rejected: Vec<RejectedStudyItemDto>,
    dispatched: bool,
) -> GenerateStudyArtifactsResponse {
    GenerateStudyArtifactsResponse {
        tool_id: "flashcards".to_owned(),
        tool_version: 1,
        transcript_revision_id: REVISION_ID.to_owned(),
        artifacts,
        rejected,
        output_ids: vec![OUTPUT_ID.to_owned()],
        tool_run_id: dispatched.then(|| TOOL_RUN_ID.to_owned()),
        dispatched,
    }
}

pub fn study_sets(
    artifacts: StudyArtifactsDto,
    revision_is_current: bool,
) -> ListStudyArtifactsResponse {
    ListStudyArtifactsResponse {
        sets: vec![StudyArtifactSetDto {
            output_id: OUTPUT_ID.to_owned(),
            tool_id: "flashcards".to_owned(),
            tool_version: 1,
            transcript_revision_id: REVISION_ID.to_owned(),
            revision_is_current,
            created_at: "2026-02-01T13:00:00Z".to_owned(),
            artifacts,
        }],
    }
}

pub fn tool_run(status: &str, cost: CostValue, egress_allowed: bool) -> ToolRunDto {
    ToolRunDto {
        run_id: TOOL_RUN_ID.to_owned(),
        recording_id: RECORDING_ID.to_owned(),
        transcript_revision_id: REVISION_ID.to_owned(),
        tool_id: "flashcards".to_owned(),
        tool_version: 2,
        engine: "openrouter".to_owned(),
        model_id: "vendor/whisper-large".to_owned(),
        model_version: Some("2026-01".to_owned()),
        request_family: "chat_json".to_owned(),
        prompt_version: "flashcards/2".to_owned(),
        schema_version: "flashcards.v2".to_owned(),
        input_sha256: "1111111111111111111111111111111111111111111111111111111111111111".to_owned(),
        output_sha256: Some(
            "2222222222222222222222222222222222222222222222222222222222222222".to_owned(),
        ),
        status: status.to_owned(),
        consent_policy: ConsentPolicyDto {
            egress_allowed,
            privacy_class: if egress_allowed {
                "cloud_retaining".to_owned()
            } else {
                "on_device".to_owned()
            },
            consent_id: egress_allowed.then(|| "consent-1".to_owned()),
        },
        cost,
        created_at: "2026-02-01T14:00:00Z".to_owned(),
        finished_at: Some("2026-02-01T14:00:20Z".to_owned()),
    }
}

pub fn tool_runs(runs: Vec<ToolRunDto>) -> ListToolRunsResponse {
    ListToolRunsResponse { runs }
}
