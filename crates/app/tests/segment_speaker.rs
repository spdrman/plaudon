//! `set_segment_speaker` over the real ports (spec §4.1, VP-030..VP-033).
//!
//! The T12 wire leg is only worth anything if the assignment is *stored*, so
//! every case here goes through the command functions and reads the result
//! back with `get_transcript` — once from the open library, and once from a
//! library that was closed and opened again, which is the only way to tell a
//! write from a cache.
//!
//! No interface, no network, no model: one temporary library per case.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use plaudon_notifications::testing::RecordingNotifier;
use plaudon_notifications::{Dispatcher, NotificationSettings as CrateSettings, Notifier};

use plaudon_app::commands::{import, library, speakers, transcript};
use plaudon_app::dto::{
    DuplicatePolicy, GetTranscriptRequest, ImportFilesRequest, ImportOutcome,
    ListRecordingsRequest, ListSpeakersRequest, RecordingFilter, RecordingSort, RecordingSortField,
    SaveTranscriptRevisionRequest, SegmentEdit, SetSegmentSpeakerRequest, SortDirection,
    SpeakerDto,
};
use plaudon_app::events::{EventSink, RecordingEventSink};
use plaudon_app::ports::{MachineRevisionSpec, MachineSegment};
use plaudon_app::secrets::{InMemorySecretStore, SecretStore};
use plaudon_app::{build_ports, AppPorts};
use plaudon_domain::error::AppErrorCode;
use plaudon_domain::ids::{RecordingId, SpeakerId, TranscriptRevisionId};
use plaudon_domain::transcript::AlignmentOrigin;

struct Harness {
    ports: AppPorts,
    library: tempfile::TempDir,
    sources: tempfile::TempDir,
}

impl Harness {
    async fn new() -> Self {
        let library = tempfile::tempdir().expect("tempdir");
        let sources = tempfile::tempdir().expect("tempdir");
        let ports = open(library.path(), sources.path()).await;
        Self {
            ports,
            library,
            sources,
        }
    }

    /// Close this library and open it again, which is what a restart does.
    async fn reopen(self) -> Self {
        let Self {
            ports,
            library,
            sources,
        } = self;
        drop(ports);
        let ports = open(library.path(), sources.path()).await;
        Self {
            ports,
            library,
            sources,
        }
    }

    fn source_file(&self, name: &str, bytes: &[u8]) -> String {
        let path = self.sources.path().join(name);
        std::fs::write(&path, bytes).expect("write a source file");
        path.to_string_lossy().into_owned()
    }
}

async fn open(library: &Path, sources: &Path) -> AppPorts {
    let sink = Arc::new(RecordingEventSink::new());
    let secrets = Arc::new(InMemorySecretStore::new());
    let dispatcher = Arc::new(
        Dispatcher::new(
            RecordingNotifier::granted() as Arc<dyn Notifier>,
            CrateSettings::uniform(false),
        )
        .with_timeout(Duration::from_millis(100)),
    );
    build_ports(
        library,
        Arc::clone(&sink) as Arc<dyn EventSink>,
        dispatcher,
        Arc::clone(&secrets) as Arc<dyn SecretStore>,
        vec![sources.to_path_buf()],
    )
    .await
    .expect("the library opens")
}

/// One diarized segment, which is how a speaker row comes into being.
fn diarized(text: &str, start_ms: u64, end_ms: u64, label: &str) -> MachineSegment {
    MachineSegment {
        speaker_label: Some(label.to_owned()),
        ..MachineSegment::timed(text, start_ms, end_ms)
    }
}

/// Import one file and return the recording it created.
///
/// `import_files` answers with the job that does the copying, so the id is
/// read back out of the library the way a screen would read it.
async fn imported(harness: &Harness, name: &str, bytes: &[u8]) -> RecordingId {
    let path = harness.source_file(name, bytes);
    let response = import::import_files(
        &harness.ports,
        ImportFilesRequest {
            paths: vec![path],
            duplicate_policy: DuplicatePolicy::Ask,
        },
    )
    .await
    .expect("the import is accepted");
    assert!(matches!(
        response.results[0],
        ImportOutcome::Accepted { .. }
    ));

    let title = Path::new(name)
        .file_stem()
        .expect("a file stem")
        .to_string_lossy()
        .into_owned();
    let page = library::list_recordings(
        &harness.ports,
        ListRecordingsRequest {
            cursor: None,
            limit: 50,
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
        },
    )
    .await
    .expect("the library lists");
    page.items
        .into_iter()
        .find(|item| item.title == title)
        .unwrap_or_else(|| panic!("no recording titled {title}"))
        .recording_id
}

/// A recording with two diarized segments and the two speaker rows they made.
async fn diarized_recording(
    harness: &Harness,
    name: &str,
    bytes: &[u8],
) -> (RecordingId, TranscriptRevisionId, Vec<SpeakerDto>) {
    let recording_id = imported(harness, name, bytes).await;
    let machine = harness
        .ports
        .transcript
        .create_machine_revision(MachineRevisionSpec {
            recording_id,
            segments: vec![
                diarized("welcome everyone", 0, 1_000, "Speaker 1"),
                diarized("thanks for having me", 1_000, 2_000, "Speaker 2"),
            ],
            engine_id: "whisper.cpp".into(),
            model_id: "small".into(),
            alignment_origin: AlignmentOrigin::Native,
        })
        .await
        .expect("the machine revision is stored");

    let listed = speakers::list_speakers(
        &harness.ports,
        ListSpeakersRequest {
            recording_id: recording_id.to_string(),
        },
    )
    .await
    .expect("the speaker rows");
    assert_eq!(listed.speakers.len(), 2);
    (recording_id, machine.revision_id, listed.speakers)
}

async fn page(
    ports: &AppPorts,
    revision_id: TranscriptRevisionId,
) -> Vec<(Option<String>, Option<String>)> {
    let page = transcript::get_transcript(
        ports,
        GetTranscriptRequest {
            revision_id: revision_id.to_string(),
            cursor: None,
            limit: 10,
        },
    )
    .await
    .expect("the transcript page");
    page.segments
        .into_iter()
        .map(|segment| (segment.speaker_id, segment.speaker_label))
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reassignment_is_stored_and_survives_reopening_the_library() {
    let harness = Harness::new().await;
    let (recording_id, revision_id, rows) =
        diarized_recording(&harness, "panel.wav", b"RIFF panel payload").await;
    let first = rows[0].speaker_id;
    let second = rows[1].speaker_id;

    // VP-030: the identity is on the wire next to the resolved label.
    assert_eq!(
        page(&harness.ports, revision_id).await,
        vec![
            (Some(first.to_string()), Some("Speaker 1".to_owned())),
            (Some(second.to_string()), Some("Speaker 2".to_owned())),
        ]
    );

    let answer = speakers::set_segment_speaker(
        &harness.ports,
        SetSegmentSpeakerRequest {
            recording_id: recording_id.to_string(),
            transcript_revision_id: revision_id.to_string(),
            seq_index: 0,
            speaker_id: Some(second.to_string()),
        },
    )
    .await
    .expect("the reassignment is accepted");
    assert_eq!(answer.speaker_id, Some(second));
    assert_eq!(answer.speaker_label.as_deref(), Some("Speaker 2"));
    assert_eq!(answer.seq_index, 0);
    assert_eq!(answer.recording_id, recording_id);
    assert_eq!(answer.transcript_revision_id, revision_id);

    // Clearing is a first-class answer: overlapping speech has no single
    // speaker, and the user has to be able to say so.
    speakers::set_segment_speaker(
        &harness.ports,
        SetSegmentSpeakerRequest {
            recording_id: recording_id.to_string(),
            transcript_revision_id: revision_id.to_string(),
            seq_index: 1,
            speaker_id: None,
        },
    )
    .await
    .expect("clearing an attribution is accepted");

    let expected = vec![
        (Some(second.to_string()), Some("Speaker 2".to_owned())),
        (None, None),
    ];
    assert_eq!(page(&harness.ports, revision_id).await, expected);

    // The only evidence that matters: it is still true after a restart.
    let harness = harness.reopen().await;
    assert_eq!(page(&harness.ports, revision_id).await, expected);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_speaker_from_another_recording_is_refused() {
    let harness = Harness::new().await;
    let (recording_id, revision_id, _) =
        diarized_recording(&harness, "panel.wav", b"RIFF panel payload").await;
    let (_, _, other_rows) = diarized_recording(&harness, "other.wav", b"RIFF other payload").await;

    let refused = speakers::set_segment_speaker(
        &harness.ports,
        SetSegmentSpeakerRequest {
            recording_id: recording_id.to_string(),
            transcript_revision_id: revision_id.to_string(),
            seq_index: 0,
            speaker_id: Some(other_rows[0].speaker_id.to_string()),
        },
    )
    .await
    .expect_err("a speaker of another recording is not a speaker here");
    assert_eq!(refused.code(), AppErrorCode::InvalidArgument);

    // A speaker row that exists nowhere is refused the same way.
    let unknown = speakers::set_segment_speaker(
        &harness.ports,
        SetSegmentSpeakerRequest {
            recording_id: recording_id.to_string(),
            transcript_revision_id: revision_id.to_string(),
            seq_index: 0,
            speaker_id: Some(SpeakerId::new().to_string()),
        },
    )
    .await
    .expect_err("an unknown speaker is refused");
    assert_eq!(unknown.code(), AppErrorCode::InvalidArgument);

    // And the attribution the diarizer wrote is untouched.
    let segments = page(&harness.ports, revision_id).await;
    assert_eq!(segments[0].1.as_deref(), Some("Speaker 1"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_malformed_or_absent_target_is_refused_before_anything_is_written() {
    let harness = Harness::new().await;
    let (recording_id, revision_id, rows) =
        diarized_recording(&harness, "panel.wav", b"RIFF panel payload").await;
    let request = |seq_index: u32, recording: String, revision: String| SetSegmentSpeakerRequest {
        recording_id: recording,
        transcript_revision_id: revision,
        seq_index,
        speaker_id: Some(rows[0].speaker_id.to_string()),
    };

    let malformed = speakers::set_segment_speaker(
        &harness.ports,
        request(0, "not-a-uuid".to_owned(), revision_id.to_string()),
    )
    .await
    .expect_err("a malformed id never reaches the store");
    assert_eq!(malformed.code(), AppErrorCode::InvalidArgument);

    let missing_segment = speakers::set_segment_speaker(
        &harness.ports,
        request(99, recording_id.to_string(), revision_id.to_string()),
    )
    .await
    .expect_err("a segment that does not exist cannot be attributed");
    assert_eq!(missing_segment.code(), AppErrorCode::RecordNotFound);

    let foreign_revision = speakers::set_segment_speaker(
        &harness.ports,
        request(0, RecordingId::new().to_string(), revision_id.to_string()),
    )
    .await
    .expect_err("a revision of another recording is not this recording's");
    assert_eq!(foreign_revision.code(), AppErrorCode::RecordNotFound);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_superseded_revision_cannot_be_reattributed() {
    let harness = Harness::new().await;
    let (recording_id, revision_id, rows) =
        diarized_recording(&harness, "panel.wav", b"RIFF panel payload").await;

    let saved = transcript::save_transcript_revision(
        &harness.ports,
        SaveTranscriptRevisionRequest {
            recording_id: recording_id.to_string(),
            base_revision_id: revision_id.to_string(),
            edits: vec![SegmentEdit {
                index: 0,
                text: "welcome, everyone".into(),
            }],
        },
    )
    .await
    .expect("the edit is saved as a new revision");

    let refused = speakers::set_segment_speaker(
        &harness.ports,
        SetSegmentSpeakerRequest {
            recording_id: recording_id.to_string(),
            transcript_revision_id: revision_id.to_string(),
            seq_index: 0,
            speaker_id: Some(rows[1].speaker_id.to_string()),
        },
    )
    .await
    .expect_err("history is not editable");
    assert_eq!(refused.code(), AppErrorCode::Conflict);

    // The head revision takes the same assignment.
    speakers::set_segment_speaker(
        &harness.ports,
        SetSegmentSpeakerRequest {
            recording_id: recording_id.to_string(),
            transcript_revision_id: saved.revision_id.to_string(),
            seq_index: 0,
            speaker_id: Some(rows[1].speaker_id.to_string()),
        },
    )
    .await
    .expect("the head revision is attributable");
    assert_eq!(
        page(&harness.ports, saved.revision_id).await[0]
            .1
            .as_deref(),
        Some("Speaker 2")
    );
}
