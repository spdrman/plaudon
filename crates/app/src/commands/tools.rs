//! Validated logic behind the six transcript-tool commands (spec §9.7, §9.8,
//! matrix row A11).
//!
//! The commands are deliberately thin, and the three rules they do enforce are
//! the ones a view must not be trusted with:
//!
//! * **A question is validated here.** Empty, oversized or non-printable
//!   questions are refused before a retriever ever reads the transcript.
//! * **A tool identity is validated here.** `tool_id` is a lowercase
//!   identifier, so a caller cannot reach a path, a URL or a SQL fragment
//!   through it.
//! * **A refusal outcome is not an error.** `not_supported_by_recording` and
//!   `insufficient_evidence` come back as successful responses the user can
//!   read; only a refused route, an unavailable model or an invalid argument
//!   becomes an [`AppError`].

use plaudon_domain::error::AppError;
use plaudon_domain::ids::RecordingId;

use crate::dto::{
    AskRecordingRequest, AskRecordingResponse, GenerateStudyArtifactsRequest,
    GenerateStudyArtifactsResponse, ListConversationsRequest, ListConversationsResponse,
    ListStudyArtifactsRequest, ListStudyArtifactsResponse, ListToolRunsRequest,
    ListToolRunsResponse, ListTranscriptToolsRequest, ListTranscriptToolsResponse,
};
use crate::validate::{self, parse_id};
use crate::AppPorts;

/// Every registered tool at its highest version, with the route it would run
/// on.
pub async fn list_transcript_tools(
    ports: &AppPorts,
    request: ListTranscriptToolsRequest,
) -> Result<ListTranscriptToolsResponse, AppError> {
    let recording_id: Option<RecordingId> = request
        .recording_id
        .as_deref()
        .map(|value| parse_id("recording_id", value))
        .transpose()?;
    ports.tools.list_transcript_tools(recording_id).await
}

/// Ask one question about one recording.
pub async fn ask_recording(
    ports: &AppPorts,
    request: AskRecordingRequest,
) -> Result<AskRecordingResponse, AppError> {
    let validated = validate::ask_recording(&request)?;
    ports.tools.ask_recording(validated).await
}

/// Every conversation recorded against one recording.
pub async fn list_conversations(
    ports: &AppPorts,
    request: ListConversationsRequest,
) -> Result<ListConversationsResponse, AppError> {
    let recording_id: RecordingId = parse_id("recording_id", &request.recording_id)?;
    ports.tools.list_conversations(recording_id).await
}

/// Generate one set of study artifacts and store what verified.
pub async fn generate_study_artifacts(
    ports: &AppPorts,
    request: GenerateStudyArtifactsRequest,
) -> Result<GenerateStudyArtifactsResponse, AppError> {
    let validated = validate::generate_study_artifacts(&request)?;
    ports.tools.generate_study_artifacts(validated).await
}

/// Study artifacts already stored for one recording.
pub async fn list_study_artifacts(
    ports: &AppPorts,
    request: ListStudyArtifactsRequest,
) -> Result<ListStudyArtifactsResponse, AppError> {
    let recording_id: RecordingId = parse_id("recording_id", &request.recording_id)?;
    ports.tools.list_study_artifacts(recording_id).await
}

/// Recorded tool runs of one recording.
pub async fn list_tool_runs(
    ports: &AppPorts,
    request: ListToolRunsRequest,
) -> Result<ListToolRunsResponse, AppError> {
    let recording_id: RecordingId = parse_id("recording_id", &request.recording_id)?;
    ports.tools.list_tool_runs(recording_id).await
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::path::PathBuf;
    use std::sync::Arc;

    use async_trait::async_trait;
    use parking_lot::Mutex;
    use plaudon_domain::error::AppErrorCode;
    use plaudon_notifications::testing::RecordingNotifier;
    use plaudon_notifications::{Dispatcher, NotificationSettings as CrateSettings, Notifier};

    use crate::events::{EventSink, NullEventSink};
    use crate::ports::ToolsPort;
    use crate::secrets::{InMemorySecretStore, SecretStore};
    use crate::validate::{
        ValidatedAskRecording, ValidatedGenerateStudyArtifacts, MAX_QUESTION_CHARS,
    };

    /// A port that answers nothing and remembers everything it was asked.
    ///
    /// The assertions below are about the command layer only: what it refuses
    /// before a port exists, and that what it does pass through arrives
    /// unchanged.
    #[derive(Debug, Default)]
    struct FakeTools {
        asks: Mutex<Vec<ValidatedAskRecording>>,
        studies: Mutex<Vec<ValidatedGenerateStudyArtifacts>>,
        recordings: Mutex<Vec<Option<RecordingId>>>,
    }

    #[async_trait]
    impl ToolsPort for FakeTools {
        async fn list_transcript_tools(
            &self,
            recording_id: Option<RecordingId>,
        ) -> Result<ListTranscriptToolsResponse, AppError> {
            self.recordings.lock().push(recording_id);
            Ok(ListTranscriptToolsResponse {
                tools: Vec::new(),
                text_model: None,
            })
        }

        async fn ask_recording(
            &self,
            request: ValidatedAskRecording,
        ) -> Result<AskRecordingResponse, AppError> {
            self.asks.lock().push(request.clone());
            Ok(AskRecordingResponse {
                conversation_id: uuid::Uuid::nil().to_string(),
                transcript_revision_id: uuid::Uuid::nil().to_string(),
                outcome: "not_supported_by_recording".to_owned(),
                text: "This recording does not discuss that.".to_owned(),
                anchors: Vec::new(),
                rejected_anchors: Vec::new(),
                retrieved_segments: Vec::new(),
                dispatched: false,
                tool_run_id: None,
                tool_id: "ask_recording".to_owned(),
                tool_version: 1,
            })
        }

        async fn list_conversations(
            &self,
            recording_id: RecordingId,
        ) -> Result<ListConversationsResponse, AppError> {
            self.recordings.lock().push(Some(recording_id));
            Ok(ListConversationsResponse {
                conversations: Vec::new(),
            })
        }

        async fn generate_study_artifacts(
            &self,
            request: ValidatedGenerateStudyArtifacts,
        ) -> Result<GenerateStudyArtifactsResponse, AppError> {
            self.studies.lock().push(request.clone());
            Ok(GenerateStudyArtifactsResponse {
                tool_id: request.tool_id,
                tool_version: 1,
                transcript_revision_id: uuid::Uuid::nil().to_string(),
                artifacts: crate::dto::StudyArtifactsDto::default(),
                rejected: Vec::new(),
                output_ids: Vec::new(),
                tool_run_id: None,
                dispatched: false,
            })
        }

        async fn list_study_artifacts(
            &self,
            recording_id: RecordingId,
        ) -> Result<ListStudyArtifactsResponse, AppError> {
            self.recordings.lock().push(Some(recording_id));
            Ok(ListStudyArtifactsResponse { sets: Vec::new() })
        }

        async fn list_tool_runs(
            &self,
            recording_id: RecordingId,
        ) -> Result<ListToolRunsResponse, AppError> {
            self.recordings.lock().push(Some(recording_id));
            Ok(ListToolRunsResponse { runs: Vec::new() })
        }
    }

    /// Real ports over a temporary library, with the tools port replaced.
    async fn ports() -> (AppPorts, Arc<FakeTools>, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let notifier = RecordingNotifier::granted();
        let dispatcher = Arc::new(Dispatcher::new(
            Arc::clone(&notifier) as Arc<dyn Notifier>,
            CrateSettings::uniform(false),
        ));
        let ports = crate::build_ports(
            dir.path(),
            Arc::new(NullEventSink) as Arc<dyn EventSink>,
            dispatcher,
            Arc::new(InMemorySecretStore::new()) as Arc<dyn SecretStore>,
            Vec::<PathBuf>::new(),
        )
        .await
        .expect("the library opens");
        let fake = Arc::new(FakeTools::default());
        let ports = AppPorts {
            tools: Arc::clone(&fake) as Arc<dyn ToolsPort>,
            ..ports
        };
        (ports, fake, dir)
    }

    fn ask(recording_id: &str, question: &str) -> AskRecordingRequest {
        AskRecordingRequest {
            recording_id: recording_id.to_owned(),
            question: question.to_owned(),
            revision_id: None,
            model_override: None,
            cost_acknowledged: false,
        }
    }

    fn study(recording_id: &str, tool_id: &str) -> GenerateStudyArtifactsRequest {
        GenerateStudyArtifactsRequest {
            recording_id: recording_id.to_owned(),
            tool_id: tool_id.to_owned(),
            revision_id: None,
            model_override: None,
            cost_acknowledged: false,
        }
    }

    const RECORDING: &str = "018f3f4e-0000-7000-8000-000000000001";

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_empty_or_oversized_question_never_reaches_the_port() {
        let (ports, fake, _dir) = ports().await;
        for question in ["", "   ", "\u{9}"] {
            let error = ask_recording(&ports, ask(RECORDING, question))
                .await
                .expect_err("an empty question is refused");
            assert_eq!(error.code(), AppErrorCode::InvalidArgument, "{question:?}");
        }
        let long = "a".repeat(MAX_QUESTION_CHARS + 1);
        let error = ask_recording(&ports, ask(RECORDING, &long))
            .await
            .expect_err("an oversized question is refused");
        assert_eq!(error.code(), AppErrorCode::InvalidArgument);
        assert!(fake.asks.lock().is_empty(), "the port was reached anyway");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_recording_id_that_is_not_a_uuid_is_refused_by_every_command() {
        let (ports, fake, _dir) = ports().await;
        for bad in ["", "../../etc/passwd", "1; DROP TABLE recordings", "rec-1"] {
            assert_eq!(
                ask_recording(&ports, ask(bad, "when does it ship?"))
                    .await
                    .expect_err("refused")
                    .code(),
                AppErrorCode::InvalidArgument,
                "{bad:?}"
            );
            assert_eq!(
                generate_study_artifacts(&ports, study(bad, "flashcards"))
                    .await
                    .expect_err("refused")
                    .code(),
                AppErrorCode::InvalidArgument,
                "{bad:?}"
            );
            for result in [
                list_conversations(
                    &ports,
                    ListConversationsRequest {
                        recording_id: bad.to_owned(),
                    },
                )
                .await
                .map(|_| ()),
                list_study_artifacts(
                    &ports,
                    ListStudyArtifactsRequest {
                        recording_id: bad.to_owned(),
                    },
                )
                .await
                .map(|_| ()),
                list_tool_runs(
                    &ports,
                    ListToolRunsRequest {
                        recording_id: bad.to_owned(),
                    },
                )
                .await
                .map(|_| ()),
                list_transcript_tools(
                    &ports,
                    ListTranscriptToolsRequest {
                        recording_id: Some(bad.to_owned()),
                    },
                )
                .await
                .map(|_| ()),
            ] {
                assert_eq!(
                    result.expect_err("refused").code(),
                    AppErrorCode::InvalidArgument,
                    "{bad:?}"
                );
            }
        }
        assert!(fake.asks.lock().is_empty(), "no ask may be dispatched");
        assert!(fake.studies.lock().is_empty(), "no study may be dispatched");
        assert!(
            fake.recordings.lock().is_empty(),
            "no recording may be read"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_hostile_tool_id_cannot_reach_a_path_a_shell_or_a_query() {
        let (ports, fake, _dir) = ports().await;
        for bad in [
            "",
            "../../etc/passwd",
            "flashcards; DROP TABLE tool_runs",
            "Flashcards",
            "flash cards",
            "flashcards/../quiz",
            &"a".repeat(crate::validate::MAX_TOOL_ID_CHARS + 1),
        ] {
            let error = generate_study_artifacts(&ports, study(RECORDING, bad))
                .await
                .expect_err("a hostile tool id is refused");
            assert_eq!(error.code(), AppErrorCode::InvalidArgument, "{bad:?}");
        }
        assert!(fake.studies.lock().is_empty(), "no study may be dispatched");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_valid_request_reaches_the_port_unchanged() {
        let (ports, fake, _dir) = ports().await;
        let response = ask_recording(&ports, ask(RECORDING, "when does the migration ship?"))
            .await
            .expect("a valid question is accepted");
        assert_eq!(response.outcome, "not_supported_by_recording");

        let asks = fake.asks.lock().clone();
        assert_eq!(asks.len(), 1);
        assert_eq!(asks[0].recording_id.to_string(), RECORDING);
        assert_eq!(asks[0].question, "when does the migration ship?");
        assert_eq!(asks[0].revision_id, None);
        assert_eq!(asks[0].model_override, None);
        assert!(!asks[0].cost_acknowledged);

        generate_study_artifacts(&ports, study(RECORDING, "flashcards"))
            .await
            .expect("a valid study request is accepted");
        let studies = fake.studies.lock().clone();
        assert_eq!(studies.len(), 1);
        assert_eq!(studies[0].tool_id, "flashcards");
        assert_eq!(studies[0].recording_id.to_string(), RECORDING);

        list_transcript_tools(
            &ports,
            ListTranscriptToolsRequest {
                recording_id: Some(RECORDING.to_owned()),
            },
        )
        .await
        .expect("listing is accepted");
        assert_eq!(
            fake.recordings
                .lock()
                .first()
                .and_then(|id| *id)
                .map(|id| id.to_string()),
            Some(RECORDING.to_owned())
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn listing_tools_without_a_recording_passes_no_recording_at_all() {
        let (ports, fake, _dir) = ports().await;
        list_transcript_tools(&ports, ListTranscriptToolsRequest { recording_id: None })
            .await
            .expect("listing is accepted");
        assert_eq!(fake.recordings.lock().as_slice(), [None]);
    }

    /* ------------------------------------------------------ schema conformance */

    /// Check one payload against its published schema.
    ///
    /// The schemas are the contract the interface codes against; a DTO that
    /// drifts from one is a wire break nothing else would catch until the
    /// other side failed to parse it.
    fn validate_against(name: &str, instance: &impl serde::Serialize) {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../schemas/commands/v1")
            .join(format!("{name}.schema.json"));
        let raw = std::fs::read_to_string(&path).expect("the schema exists");
        let schema: serde_json::Value = serde_json::from_str(&raw).expect("valid json");
        let validator = jsonschema::validator_for(&schema).expect("the schema compiles");
        let value = serde_json::to_value(instance).expect("serializes");
        let errors: Vec<String> = validator
            .iter_errors(&value)
            .map(|error| format!("{error} at {}", error.instance_path))
            .collect();
        assert!(errors.is_empty(), "{name}: {}\n{value}", errors.join("\n"));
    }

    fn anchor_dto() -> crate::dto::AnchorDto {
        crate::dto::AnchorDto {
            seq_index: 0,
            char_start: 0,
            char_end: 7,
            excerpt: "We will".to_owned(),
            verified: true,
            rejection_code: None,
            matched_text: Some("We will".to_owned()),
            alignment_origin: "native".to_owned(),
            seek_ms: Some(0),
        }
    }

    fn artifacts() -> crate::dto::StudyArtifactsDto {
        crate::dto::StudyArtifactsDto {
            flashcards: vec![crate::dto::FlashcardDto {
                front: "What is idempotency?".to_owned(),
                back: "Retry safety.".to_owned(),
                anchors: vec![anchor_dto()],
            }],
            quiz: vec![crate::dto::QuizItemDto {
                question: "When does it ship?".to_owned(),
                options: vec!["Monday".to_owned(), "Friday".to_owned()],
                answer_index: 1,
                anchors: vec![anchor_dto()],
            }],
            definitions: vec![crate::dto::DefinitionDto {
                term: "Idempotency".to_owned(),
                definition: "A retry that duplicates nothing.".to_owned(),
                anchors: vec![anchor_dto()],
            }],
        }
    }

    #[test]
    fn every_tool_request_and_response_matches_its_published_schema() {
        validate_against(
            "list_transcript_tools.request",
            &ListTranscriptToolsRequest {
                recording_id: Some(RECORDING.to_owned()),
            },
        );
        validate_against(
            "list_transcript_tools.response",
            &ListTranscriptToolsResponse {
                tools: vec![crate::dto::TranscriptToolDto {
                    tool_id: "flashcards".to_owned(),
                    version: 1,
                    kind: "flashcards".to_owned(),
                    display_name: "Study flashcards".to_owned(),
                    instruction: "Produce study flashcards.".to_owned(),
                    prompt_version: "flashcards/1".to_owned(),
                    output_schema_version: "flashcards.v1".to_owned(),
                    origin: "built_in".to_owned(),
                    optional: true,
                    offerable: false,
                    unavailable_reason: Some("the model's output budget is unknown".to_owned()),
                }],
                text_model: Some(crate::dto::TextRouteDto {
                    engine: "apple-foundation".to_owned(),
                    model_id: Some("system-language-model".to_owned()),
                    is_cloud: false,
                    available: true,
                    unavailable_reason: None,
                }),
            },
        );
        validate_against(
            "ask_recording.request",
            &ask(RECORDING, "when does the migration ship?"),
        );
        validate_against(
            "ask_recording.response",
            &AskRecordingResponse {
                conversation_id: RECORDING.to_owned(),
                transcript_revision_id: RECORDING.to_owned(),
                outcome: "answered".to_owned(),
                text: "On Friday.".to_owned(),
                anchors: vec![anchor_dto()],
                rejected_anchors: vec![crate::dto::RejectedAnchorDto {
                    seq_index: 1,
                    char_start: 0,
                    char_end: 8,
                    excerpt: "Invented".to_owned(),
                    rejection_code: "excerpt_mismatch".to_owned(),
                }],
                retrieved_segments: vec![0, 1],
                dispatched: true,
                tool_run_id: Some(RECORDING.to_owned()),
                tool_id: "ask_recording".to_owned(),
                tool_version: 1,
            },
        );
        validate_against(
            "list_conversations.request",
            &ListConversationsRequest {
                recording_id: RECORDING.to_owned(),
            },
        );
        validate_against(
            "list_conversations.response",
            &ListConversationsResponse {
                conversations: vec![crate::dto::ConversationDto {
                    conversation_id: RECORDING.to_owned(),
                    recording_id: RECORDING.to_owned(),
                    transcript_revision_id: RECORDING.to_owned(),
                    revision_is_current: true,
                    created_at: "1970-01-01T00:00:00Z".to_owned(),
                    turns: vec![crate::dto::ConversationTurnDto {
                        message_id: RECORDING.to_owned(),
                        seq_index: 1,
                        role: "assistant".to_owned(),
                        text: "On Friday.".to_owned(),
                        outcome: Some("answered".to_owned()),
                        anchors: vec![anchor_dto()],
                        tool_run_id: Some(RECORDING.to_owned()),
                        created_at: "1970-01-01T00:00:00Z".to_owned(),
                    }],
                }],
            },
        );
        validate_against(
            "generate_study_artifacts.request",
            &study(RECORDING, "flashcards"),
        );
        validate_against(
            "generate_study_artifacts.response",
            &GenerateStudyArtifactsResponse {
                tool_id: "flashcards".to_owned(),
                tool_version: 1,
                transcript_revision_id: RECORDING.to_owned(),
                artifacts: artifacts(),
                rejected: vec![crate::dto::RejectedStudyItemDto {
                    path: "/flashcards/1".to_owned(),
                    reason: "uncited".to_owned(),
                    detail: "the item carried no citation at all".to_owned(),
                }],
                output_ids: vec![RECORDING.to_owned()],
                tool_run_id: Some(RECORDING.to_owned()),
                dispatched: false,
            },
        );
        validate_against(
            "list_study_artifacts.request",
            &ListStudyArtifactsRequest {
                recording_id: RECORDING.to_owned(),
            },
        );
        validate_against(
            "list_study_artifacts.response",
            &ListStudyArtifactsResponse {
                sets: vec![crate::dto::StudyArtifactSetDto {
                    output_id: RECORDING.to_owned(),
                    tool_id: "flashcards".to_owned(),
                    tool_version: 1,
                    transcript_revision_id: RECORDING.to_owned(),
                    revision_is_current: false,
                    created_at: "1970-01-01T00:00:00Z".to_owned(),
                    artifacts: artifacts(),
                }],
            },
        );
        validate_against(
            "list_tool_runs.request",
            &ListToolRunsRequest {
                recording_id: RECORDING.to_owned(),
            },
        );
        validate_against(
            "list_tool_runs.response",
            &ListToolRunsResponse {
                runs: vec![crate::dto::ToolRunDto {
                    run_id: RECORDING.to_owned(),
                    recording_id: RECORDING.to_owned(),
                    transcript_revision_id: RECORDING.to_owned(),
                    tool_id: "ask_recording".to_owned(),
                    tool_version: 1,
                    engine: "openrouter".to_owned(),
                    model_id: "vendor/model-a".to_owned(),
                    model_version: None,
                    request_family: "unsupported".to_owned(),
                    prompt_version: "ask_recording/1".to_owned(),
                    schema_version: "ask_recording.v1".to_owned(),
                    input_sha256: "a".repeat(64),
                    output_sha256: Some("b".repeat(64)),
                    status: "succeeded".to_owned(),
                    consent_policy: crate::dto::ConsentPolicyDto {
                        egress_allowed: true,
                        privacy_class: "cloud_zero_retention".to_owned(),
                        consent_id: Some("consent-1".to_owned()),
                    },
                    cost: crate::dto::CostValue::unknown(),
                    created_at: "1970-01-01T00:00:00Z".to_owned(),
                    finished_at: Some("1970-01-01T00:00:01Z".to_owned()),
                }],
            },
        );
    }
}
