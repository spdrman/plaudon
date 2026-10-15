//! Issue #305: transcribing with no speech model installed.
//!
//! The released build answered "The selected model is not available on this
//! machine." with no way forward. The failure has to say plainly that no
//! speech model is installed and carry the action that installs or imports
//! one, so the view can offer both straight from the error.

use std::sync::Arc;

use plaudon_app::command::{AppCommand, AppResponse};
use plaudon_app::dto::{
    DuplicatePolicy, ImportFilesRequest, ImportOutcome, ListRecordingsRequest, RecordingFilter,
    RecordingSort, RecordingSortField, SortDirection, StartProcessingRequest,
};
use plaudon_app::notify::UnsupportedNotifier;
use plaudon_app::runtime::{AppEngine, EngineConfig};
use plaudon_app::secrets::InMemorySecretStore;
use plaudon_domain::error::{AppErrorCode, ErrorAction};

struct Harness {
    engine: AppEngine,
    sources: tempfile::TempDir,
    _library: tempfile::TempDir,
}

impl Harness {
    /// A library with no speech engine substituted: the host's own engines,
    /// which on a machine with no whisper model and no Apple bridge are none.
    fn start() -> Self {
        let library = tempfile::tempdir().expect("library");
        let sources = tempfile::tempdir().expect("sources");
        let engine = AppEngine::start(
            EngineConfig::new(library.path().to_path_buf())
                .with_approved_roots(vec![sources.path().to_path_buf()])
                .with_secrets(Arc::new(InMemorySecretStore::new()))
                .with_notifier(Arc::new(UnsupportedNotifier))
                .with_worker_threads(2),
        )
        .expect("engine starts");
        Self {
            engine,
            sources,
            _library: library,
        }
    }

    fn import(&self) -> String {
        let path = self.sources.path().join("meeting.wav");
        std::fs::write(&path, b"RIFF a recording nobody can transcribe yet").unwrap();
        let AppResponse::ImportFiles(response) = self
            .engine
            .dispatcher()
            .call_blocking(AppCommand::ImportFiles(ImportFilesRequest {
                paths: vec![path.to_string_lossy().into_owned()],
                duplicate_policy: DuplicatePolicy::Ask,
            }))
            .expect("import is accepted")
        else {
            panic!("wrong response");
        };
        assert!(
            matches!(response.results[0], ImportOutcome::Accepted { .. }),
            "the recording was not imported: {:?}",
            response.results
        );
        let AppResponse::ListRecordings(page) = self
            .engine
            .dispatcher()
            .call_blocking(AppCommand::ListRecordings(ListRecordingsRequest {
                cursor: None,
                limit: 10,
                filter: RecordingFilter {
                    query: None,
                    import_status: Vec::new(),
                    transcription_status: Vec::new(),
                    sources: Vec::new(),
                    tags: Vec::new(),
                    date_from: None,
                    date_to: None,
                },
                sort: RecordingSort {
                    field: RecordingSortField::Title,
                    direction: SortDirection::Asc,
                },
            }))
            .expect("the library lists")
        else {
            panic!("wrong response");
        };
        page.items[0].recording_id.to_string()
    }
}

fn transcribe(recording_id: String) -> AppCommand {
    AppCommand::StartProcessing(StartProcessingRequest {
        recording_id,
        stage: "transcribe".into(),
        model_override: None,
        cost_acknowledged: false,
        rerun: false,
    })
}

#[test]
fn issue_305_transcribing_with_no_speech_model_says_so_and_offers_the_install() {
    let harness = Harness::start();
    let recording_id = harness.import();

    let envelope = harness
        .engine
        .dispatcher()
        .call_blocking(transcribe(recording_id))
        .expect_err("there is nothing to transcribe with");

    assert_eq!(envelope.code, AppErrorCode::NoSpeechModel, "{envelope:?}");
    assert_eq!(
        envelope.action,
        Some(ErrorAction::InstallSpeechModel),
        "the error must carry the install/import affordance"
    );
    assert!(!envelope.retryable, "retrying unchanged cannot succeed");
}

#[test]
fn issue_305_the_message_names_the_missing_speech_model_not_a_selected_one() {
    let harness = Harness::start();
    let recording_id = harness.import();

    let envelope = harness
        .engine
        .dispatcher()
        .call_blocking(transcribe(recording_id))
        .expect_err("there is nothing to transcribe with");

    assert!(
        !envelope.message.contains("selected model"),
        "the user never selected a model: {}",
        envelope.message
    );
    assert!(
        envelope.message.contains("No speech model is installed"),
        "{}",
        envelope.message
    );
    assert!(
        envelope.message.contains("Install a model"),
        "{}",
        envelope.message
    );
    assert!(
        envelope.message.contains("import a model file"),
        "{}",
        envelope.message
    );
}

#[test]
fn issue_305_the_no_speech_model_code_is_part_of_the_published_contract() {
    assert_eq!(AppErrorCode::NoSpeechModel.as_str(), "NO_SPEECH_MODEL");
    assert_eq!(
        AppErrorCode::NoSpeechModel.action(),
        Some(ErrorAction::InstallSpeechModel)
    );
    let schema = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../schemas/error-envelope.v1.schema.json"),
    )
    .unwrap();
    assert!(schema.contains("\"NO_SPEECH_MODEL\""));
    assert!(schema.contains("\"INSTALL_SPEECH_MODEL\""));
}
