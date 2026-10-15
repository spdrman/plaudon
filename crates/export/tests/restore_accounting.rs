//! Spec 18.9 archive accounting: what a restore writes, what it refuses, and
//! what it leaves on disk when it refuses.
//!
//! Every test runs against a real migrated SQLite library and a real
//! content-addressed media archive on a temporary volume. The assertions are
//! deliberately two-sided: the typed error *and* the state the library is left
//! in, because "the restore failed" and "the restore failed without applying
//! half of itself" are different claims and only the second one is the
//! promise spec 13.8 makes.

use std::path::{Path, PathBuf};

use plaudon_domain::time::Timestamp;
use plaudon_export::archive::crypto::KdfProfile;
use plaudon_export::archive::documents::{
    AnnotationDocument, MediaAssetRow, MediaRevisionRow, OutputDocument, OutputRow, PersonDocument,
    RecordDocument, RecordingMediaRow, RecordingRow, SegmentRow, SourceDocument, SourceItemRow,
    SpeakerRow, ToolDocument, TranscriptAnnotationRow, TranscriptDocument, TranscriptRow, WordRow,
    ANNOTATION_SCHEMA, OUTPUT_SCHEMA, PERSON_SCHEMA, RECORD_SCHEMA, SOURCE_SCHEMA, TOOL_SCHEMA,
    TRANSCRIPT_SCHEMA,
};
use plaudon_export::archive::restore::{ConflictPolicy, RestoreMode, RestoreOptions, Restorer};
use plaudon_export::archive::secrets::ArchiveSecretScanner;
use plaudon_export::archive::voiceprofiles::{
    EnrollmentRow, ExemplarRow, VoiceProfileDocument, VoiceProfilePermit, VOICEPROFILE_SCHEMA,
};
use plaudon_export::archive::writer::{ArchiveBuilder, Protection};
use plaudon_export::destination::ExportDestination;
use plaudon_export::error::{ArchiveRejection, ExportError};
use plaudon_persistence::archive::MediaArchive;
use plaudon_persistence::migrations::MigrationRunner;
use plaudon_persistence::Database;
use sha2::{Digest as _, Sha256};
use sqlx::Row as _;

const PASSWORD: &str = "correct horse battery staple";
const RECORDING: &str = "11111111-1111-4111-8111-111111111111";
const REVISION: &str = "22222222-2222-4222-8222-222222222222";
const ASSET: &str = "33333333-3333-4333-8333-333333333333";
const PERSON: &str = "44444444-4444-4444-8444-444444444444";

// ---------------------------------------------------------------------------
// A library, and the pieces an archive is assembled from
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct Library {
    root: PathBuf,
    db: Database,
    media: MediaArchive,
}

impl Library {
    async fn open(root: &Path) -> Self {
        std::fs::create_dir_all(root).unwrap();
        let db = Database::open(root.join("library.sqlite")).await.unwrap();
        MigrationRunner::new().run(&db).await.unwrap();
        let media = MediaArchive::open(root.join("media")).unwrap();
        Self {
            root: root.to_path_buf(),
            db,
            media,
        }
    }

    fn restorer(&self) -> Restorer {
        Restorer::new(self.db.clone(), self.media.clone(), self.root.clone())
    }

    async fn count(&self, table: &str) -> i64 {
        let sql = format!("SELECT COUNT(*) FROM {table}");
        sqlx::query(sqlx::AssertSqlSafe(sql))
            .fetch_one(self.db.pool())
            .await
            .unwrap()
            .get::<i64, _>(0)
    }

    async fn insert_recording(&self, id: &str, deleted_at: Option<i64>) {
        sqlx::query(
            "INSERT INTO recordings (id, title, created_at, deleted_at) VALUES (?1, 'Local', 1, ?2)",
        )
        .bind(id)
        .bind(deleted_at)
        .execute(self.db.pool())
        .await
        .unwrap();
    }
}

/// One thing an archive carries.
#[derive(Debug)]
enum Item {
    Document {
        path: String,
        schema: &'static str,
        bytes: Vec<u8>,
    },
    Media {
        path: String,
        source: PathBuf,
        sha256: String,
        byte_length: u64,
    },
    Voice {
        path: String,
        bytes: Vec<u8>,
    },
}

fn document<T: serde::Serialize>(path: &str, schema: &'static str, value: &T) -> Item {
    Item::Document {
        path: path.to_owned(),
        schema,
        bytes: serde_json::to_vec(value).unwrap(),
    }
}

fn sha_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// Write `bytes` into `work/blobs` and describe them as a media entry.
fn media_item(work: &Path, bytes: &[u8]) -> (Item, String) {
    let digest = sha_hex(bytes);
    let blobs = work.join("blobs");
    std::fs::create_dir_all(&blobs).unwrap();
    let source = blobs.join(format!("{digest}.wav"));
    std::fs::write(&source, bytes).unwrap();
    (
        Item::Media {
            path: format!("media/{digest}.wav"),
            source,
            sha256: digest.clone(),
            byte_length: bytes.len() as u64,
        },
        digest,
    )
}

fn build_archive(work: &Path, name: &str, items: Vec<Item>, protection: &Protection) -> PathBuf {
    let staging = work.join("staging");
    let out = work.join("out");
    std::fs::create_dir_all(&staging).unwrap();
    std::fs::create_dir_all(&out).unwrap();

    let mut builder = ArchiveBuilder::new(&staging, Timestamp::from_millis(1)).unwrap();
    for item in items {
        match item {
            Item::Document {
                path,
                schema,
                bytes,
            } => builder.add_document(&path, schema, &bytes).unwrap(),
            Item::Media {
                path,
                source,
                sha256,
                byte_length,
            } => builder
                .add_media(&path, &source, &sha256, byte_length)
                .unwrap(),
            Item::Voice { path, bytes } => {
                let permit = VoiceProfilePermit::for_protection(protection).unwrap();
                builder.add_voice_document(&path, &bytes, permit).unwrap();
            }
        }
    }

    let destination = ExportDestination::new(&out).unwrap();
    builder.finish(&destination, name, protection).unwrap().path
}

fn encrypted() -> Protection {
    Protection::Encrypted {
        password: PASSWORD.to_owned(),
        profile: KdfProfile::Test,
    }
}

// --------------------------------------------------------------- documents --

fn asset_row(digest: &str, byte_length: u64) -> MediaAssetRow {
    MediaAssetRow {
        id: ASSET.to_owned(),
        sha256: digest.to_owned(),
        byte_length,
        extension: "wav".to_owned(),
        mime: Some("audio/wav".to_owned()),
        codec: Some("pcm_s16le".to_owned()),
        sample_rate: Some(16_000),
        channels: Some(1),
        duration_ms: Some(1_000),
        quality_class: "user_supplied_file".to_owned(),
        created_at: 10,
    }
}

fn record_doc(recording_id: &str, assets: Vec<MediaAssetRow>) -> RecordDocument {
    let recording_media = assets
        .iter()
        .map(|asset| RecordingMediaRow {
            media_asset_id: asset.id.clone(),
            quality_class: asset.quality_class.clone(),
            obtained_at: 11,
        })
        .collect();
    let preferred = assets.first().map(|asset| asset.id.clone());
    RecordDocument {
        schema_version: RECORD_SCHEMA.to_owned(),
        recording: RecordingRow {
            id: recording_id.to_owned(),
            captured_at: Some(5),
            duration_ms: Some(1_000),
            preferred_media_asset_id: preferred,
            created_at: 10,
            deleted_at: None,
        },
        media_assets: assets,
        recording_media,
        media_revisions: Vec::new(),
        source_items: Vec::new(),
    }
}

fn annotation_doc(recording_id: &str, speakers: Vec<SpeakerRow>) -> AnnotationDocument {
    AnnotationDocument {
        schema_version: ANNOTATION_SCHEMA.to_owned(),
        recording_id: recording_id.to_owned(),
        title: "Quarterly planning".to_owned(),
        annotation: Some("Agreed the budget".to_owned()),
        tags: vec!["budget".to_owned(), "planning".to_owned()],
        speakers,
    }
}

fn speaker_row(id: &str, person_id: Option<&str>) -> SpeakerRow {
    SpeakerRow {
        id: id.to_owned(),
        label: "Speaker 1".to_owned(),
        name: Some("Dana".to_owned()),
        provenance: "user_confirmed".to_owned(),
        user_confirmed: true,
        person_id: person_id.map(str::to_owned),
    }
}

fn transcript_doc(recording_id: &str) -> TranscriptDocument {
    TranscriptDocument {
        schema_version: TRANSCRIPT_SCHEMA.to_owned(),
        recording_id: recording_id.to_owned(),
        revision: TranscriptRow {
            id: REVISION.to_owned(),
            parent_id: None,
            origin: "machine".to_owned(),
            input_sha256: "a".repeat(64),
            engine: "whisper.cpp".to_owned(),
            model_id: "tiny".to_owned(),
            model_version: Some("1".to_owned()),
            locale: Some("en".to_owned()),
            alignment_origin: "native".to_owned(),
            status: "complete".to_owned(),
            normalization_profile: "default".to_owned(),
            chunking_strategy: "none".to_owned(),
            duration_ms: Some(1_000),
            created_at: 12,
            engine_backend: Some("cpu".to_owned()),
            engine_params_json: Some("{}".to_owned()),
            source_vendor: None,
        },
        segments: vec![SegmentRow {
            id: "55555555-5555-4555-8555-555555555555".to_owned(),
            seq_index: 0,
            start_ms: Some(0),
            end_ms: Some(1_000),
            speaker_id: None,
            text: "We agreed the budget.".to_owned(),
            confidence: Some(0.9),
            confidence_scale: Some("engine".to_owned()),
            alignment_origin: "native".to_owned(),
            words: vec![WordRow {
                id: "66666666-6666-4666-8666-666666666666".to_owned(),
                seq_index: 0,
                start_ms: 0,
                end_ms: 400,
                text: "We".to_owned(),
                confidence: Some(0.9),
                confidence_scale: Some("engine".to_owned()),
            }],
        }],
        annotations: vec![TranscriptAnnotationRow {
            id: "77777777-7777-4777-8777-777777777777".to_owned(),
            segment_index: 0,
            kind: "low_confidence".to_owned(),
            reason: "the engine reported a low score".to_owned(),
            start_ms: Some(0),
            end_ms: Some(1_000),
        }],
    }
}

fn output_doc(recording_id: &str) -> OutputDocument {
    OutputDocument {
        schema_version: OUTPUT_SCHEMA.to_owned(),
        recording_id: recording_id.to_owned(),
        output: OutputRow {
            id: "88888888-8888-4888-8888-888888888888".to_owned(),
            output_type: "meeting_summary".to_owned(),
            transcript_revision_id: REVISION.to_owned(),
            template_version: "1".to_owned(),
            model_id: "local".to_owned(),
            prompt_version: "1".to_owned(),
            schema_version: "meeting-summary/1".to_owned(),
            body_json: r#"{"summary":"We agreed the budget."}"#.to_owned(),
            status: "complete".to_owned(),
            created_at: 13,
        },
        claims: Vec::new(),
        evidence: Vec::new(),
    }
}

fn source_doc(id: &str) -> SourceDocument {
    SourceDocument {
        schema_version: SOURCE_SCHEMA.to_owned(),
        id: id.to_owned(),
        source_kind: "local_file".to_owned(),
        account_scope: "local".to_owned(),
        display_name: "Files".to_owned(),
        status: "ready".to_owned(),
        sync_policy: "manual".to_owned(),
        created_at: 9,
    }
}

fn tool_doc(id: &str) -> ToolDocument {
    ToolDocument {
        schema_version: TOOL_SCHEMA.to_owned(),
        id: id.to_owned(),
        version: 1,
        // Sixty-four characters, as the schema requires, but not a bare hex
        // run: the shared credential table treats an unqualified 64-hex value
        // as a device token, and `definition_sha256` is not one of the
        // scanner's documented digest keys. The companion test below pins that
        // behaviour so this choice is not folklore.
        definition_sha256: format!("tool-definition-{}", "z".repeat(48)),
        origin: "builtin".to_owned(),
        optional: 0,
        created_at: 8,
        display_name: "Action items".to_owned(),
        kind: "extraction".to_owned(),
        prompt_version: "1".to_owned(),
        output_schema_version: "action-items/1".to_owned(),
    }
}

fn person_doc(id: &str) -> PersonDocument {
    PersonDocument {
        schema_version: PERSON_SCHEMA.to_owned(),
        id: id.to_owned(),
        display_name: "Dana Scully".to_owned(),
        aliases: vec!["Dana".to_owned()],
        email: Some("dana@example.test".to_owned()),
        organisation: Some("Field Office".to_owned()),
        status: "active".to_owned(),
        created_at: 7,
        updated_at: 7,
    }
}

fn voice_doc(person_id: &str, recording_id: Option<&str>) -> VoiceProfileDocument {
    use base64::engine::general_purpose::STANDARD as BASE64;
    use base64::Engine as _;

    VoiceProfileDocument {
        schema_version: VOICEPROFILE_SCHEMA.to_owned(),
        person_id: person_id.to_owned(),
        enrollment: EnrollmentRow {
            consent_version: "voice-consent/1".to_owned(),
            consent_note: Some("asked in the meeting".to_owned()),
            retention_months: 6,
            last_matched_at: None,
            status: "active".to_owned(),
            expired_at: None,
            created_at: 14,
        },
        exemplars: vec![ExemplarRow {
            id: "99999999-9999-4999-8999-999999999999".to_owned(),
            recording_id: recording_id.map(str::to_owned),
            cluster_key: Some("cluster-1".to_owned()),
            source_start_ms: Some(0),
            source_end_ms: Some(2_000),
            speech_ms: 2_000,
            model_id: "ecapa".to_owned(),
            model_version: "1".to_owned(),
            model_sha256: "b".repeat(64),
            embedding_dim: 192,
            embedding_cipher_b64: BASE64.encode([7u8; 64]),
            embedding_nonce_b64: BASE64.encode([9u8; 24]),
            created_at: 15,
        }],
    }
}

/// A complete, self-consistent archive: one recording with audio, a transcript,
/// an output, a source, a tool and a roster entry.
fn full_archive(work: &Path, name: &str) -> PathBuf {
    let (media, digest) = media_item(work, b"RIFF....WAVEfake audio bytes");
    let mut record = record_doc(RECORDING, vec![asset_row(&digest, 28)]);
    record.source_items.push(SourceItemRow {
        id: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa".to_owned(),
        source_id: "source-1".to_owned(),
        external_id: Some("external-1".to_owned()),
        import_status: "imported".to_owned(),
        last_seen_at: Some(20),
        remote_version: Some("v1".to_owned()),
        remote_state: "present".to_owned(),
        tombstone_reason: None,
        created_at: 9,
    });
    record.media_revisions.push(MediaRevisionRow {
        id: "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb".to_owned(),
        media_asset_id: ASSET.to_owned(),
        source_item_id: None,
        source_version: Some("v1".to_owned()),
        predecessor_id: None,
        obtained_at: 11,
    });

    build_archive(
        work,
        name,
        vec![
            document(
                "sources/source-1.json",
                SOURCE_SCHEMA,
                &source_doc("source-1"),
            ),
            document(
                "tools/action-items-1.json",
                TOOL_SCHEMA,
                &tool_doc("action-items"),
            ),
            document(
                &format!("people/{PERSON}.json"),
                PERSON_SCHEMA,
                &person_doc(PERSON),
            ),
            media,
            document(&format!("records/{RECORDING}.json"), RECORD_SCHEMA, &record),
            document(
                &format!("annotations/{RECORDING}.json"),
                ANNOTATION_SCHEMA,
                &annotation_doc(
                    RECORDING,
                    vec![speaker_row(
                        "cccccccc-cccc-4ccc-8ccc-cccccccccccc",
                        Some(PERSON),
                    )],
                ),
            ),
            document(
                &format!("transcripts/{REVISION}.json"),
                TRANSCRIPT_SCHEMA,
                &transcript_doc(RECORDING),
            ),
            document(
                "outputs/88888888-8888-4888-8888-888888888888.json",
                OUTPUT_SCHEMA,
                &output_doc(RECORDING),
            ),
        ],
        &Protection::Plaintext,
    )
}

// ---------------------------------------------------------------------------
// The happy side, so the refusals below mean something
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_complete_archive_restores_every_namespace_it_carries() {
    let dir = tempfile::tempdir().unwrap();
    let archive = full_archive(dir.path(), "full.recording-archive");
    let library = Library::open(&dir.path().join("library")).await;

    let report = library
        .restorer()
        .restore(
            &archive,
            None,
            &RestoreOptions {
                mode: RestoreMode::NewLibrary,
                on_conflict: ConflictPolicy::Fail,
                restore_tombstoned: false,
            },
        )
        .await
        .unwrap();

    assert_eq!(report.recordings_restored, 1);
    assert_eq!(report.transcripts_restored, 1);
    assert_eq!(report.outputs_restored, 1);
    assert_eq!(report.sources_restored, 1);
    assert_eq!(report.tools_restored, 1);
    assert_eq!(report.people_restored, 1);
    assert_eq!(report.media_committed, 1);
    assert_eq!(report.media_reused, 0);
    assert_eq!(report.voice_profiles_restored, 0);
    assert!(!report.voice_consent_required);
    assert_eq!(report.conflicts, Vec::new());

    assert_eq!(library.count("recordings").await, 1);
    assert_eq!(library.count("media_assets").await, 1);
    assert_eq!(library.count("recording_media").await, 1);
    assert_eq!(library.count("media_revisions").await, 1);
    assert_eq!(library.count("source_items").await, 1);
    assert_eq!(library.count("recording_tags").await, 2);
    assert_eq!(library.count("speakers").await, 1);
    assert_eq!(library.count("transcript_segments").await, 1);
    assert_eq!(library.count("transcript_words").await, 1);
    assert_eq!(library.count("transcript_annotations").await, 1);
    assert_eq!(library.count("ai_outputs").await, 1);

    // The title and annotation come from the annotations document, not from a
    // placeholder.
    let row = sqlx::query("SELECT title, annotation FROM recordings WHERE id = ?1")
        .bind(RECORDING)
        .fetch_one(library.db.pool())
        .await
        .unwrap();
    assert_eq!(row.get::<String, _>("title"), "Quarterly planning");
    assert_eq!(
        row.get::<Option<String>, _>("annotation").as_deref(),
        Some("Agreed the budget")
    );

    // The blob landed under its content address and verifies there.
    let relative: String = sqlx::query("SELECT relative_path FROM media_assets")
        .fetch_one(library.db.pool())
        .await
        .unwrap()
        .get(0);
    assert!(library.media.exists(&relative).unwrap());

    // The speaker kept the roster link, because the roster travelled with it.
    let person: Option<String> = sqlx::query("SELECT person_id FROM speakers")
        .fetch_one(library.db.pool())
        .await
        .unwrap()
        .get(0);
    assert_eq!(person.as_deref(), Some(PERSON));
}

#[tokio::test]
async fn restoring_the_same_archive_twice_reuses_the_blob_and_inserts_nothing_new() {
    let dir = tempfile::tempdir().unwrap();
    let archive = full_archive(dir.path(), "full.recording-archive");
    let library = Library::open(&dir.path().join("library")).await;
    let options = RestoreOptions::default();

    library
        .restorer()
        .restore(&archive, None, &options)
        .await
        .unwrap();
    let second = library
        .restorer()
        .restore(&archive, None, &options)
        .await
        .unwrap();

    // The record already exists, so it is reported rather than re-inserted,
    // and the blob already in the archive is reused byte for byte.
    assert_eq!(second.media_reused, 1);
    assert_eq!(second.media_committed, 0);
    assert_eq!(second.recordings_restored, 0);
    assert_eq!(second.conflicts.len(), 1);
    assert_eq!(second.conflicts[0].reason, "already in this library");
    assert_eq!(library.count("recordings").await, 1);
    assert_eq!(library.count("media_assets").await, 1);
}

#[tokio::test]
async fn an_archive_with_no_entries_restores_nothing_and_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let archive = build_archive(
        dir.path(),
        "empty.recording-archive",
        Vec::new(),
        &Protection::Plaintext,
    );
    let library = Library::open(&dir.path().join("library")).await;

    let report = library
        .restorer()
        .restore(&archive, None, &RestoreOptions::default())
        .await
        .unwrap();

    assert_eq!(report, plaudon_export::RestoreReport::default());
    assert_eq!(library.count("recordings").await, 0);
}

#[tokio::test]
async fn an_encrypted_archive_restores_with_its_password_and_not_without_it() {
    let dir = tempfile::tempdir().unwrap();
    let (media, digest) = media_item(dir.path(), b"RIFF....WAVE sealed");
    let archive = build_archive(
        dir.path(),
        "sealed.recording-archive",
        vec![
            media,
            document(
                &format!("records/{RECORDING}.json"),
                RECORD_SCHEMA,
                &record_doc(RECORDING, vec![asset_row(&digest, 19)]),
            ),
        ],
        &encrypted(),
    );
    let library = Library::open(&dir.path().join("library")).await;
    let options = RestoreOptions::default();

    assert!(matches!(
        library
            .restorer()
            .restore(&archive, None, &options)
            .await
            .unwrap_err(),
        ExportError::PasswordRequired
    ));
    assert!(matches!(
        library
            .restorer()
            .restore(&archive, Some("wrong"), &options)
            .await
            .unwrap_err(),
        ExportError::DecryptionFailed
    ));
    assert_eq!(library.count("recordings").await, 0);

    let report = library
        .restorer()
        .restore(&archive, Some(PASSWORD), &options)
        .await
        .unwrap();
    assert_eq!(report.recordings_restored, 1);
}

// ---------------------------------------------------------------------------
// Conflicts
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_new_library_restore_refuses_a_library_that_already_has_recordings() {
    let dir = tempfile::tempdir().unwrap();
    let archive = full_archive(dir.path(), "full.recording-archive");
    let library = Library::open(&dir.path().join("library")).await;
    library.insert_recording("local-1", None).await;

    let error = library
        .restorer()
        .restore(
            &archive,
            None,
            &RestoreOptions {
                mode: RestoreMode::NewLibrary,
                on_conflict: ConflictPolicy::Skip,
                restore_tombstoned: false,
            },
        )
        .await
        .unwrap_err();

    assert!(matches!(error, ExportError::TargetLibraryNotEmpty));
    // Refused before staging: nothing was extracted and nothing was inserted.
    assert_eq!(library.count("recordings").await, 1);
    assert_eq!(library.count("media_assets").await, 0);
    assert_eq!(library.count("sources").await, 0);
}

#[tokio::test]
async fn an_existing_recording_is_skipped_and_reported_under_the_skip_policy() {
    let dir = tempfile::tempdir().unwrap();
    let archive = full_archive(dir.path(), "full.recording-archive");
    let library = Library::open(&dir.path().join("library")).await;
    library.insert_recording(RECORDING, None).await;

    let report = library
        .restorer()
        .restore(&archive, None, &RestoreOptions::default())
        .await
        .unwrap();

    assert_eq!(report.recordings_restored, 0);
    assert_eq!(report.transcripts_restored, 0);
    assert_eq!(report.outputs_restored, 0);
    // The roster, the sources and the tools are library-wide and still travel.
    assert_eq!(report.people_restored, 1);
    assert_eq!(report.sources_restored, 1);
    assert_eq!(report.tools_restored, 1);
    assert_eq!(report.conflicts.len(), 1);
    assert_eq!(
        report.conflicts[0].entry_path,
        format!("records/{RECORDING}.json")
    );
    assert_eq!(report.conflicts[0].reason, "already in this library");

    // The local title survived; the archive did not overwrite it.
    let title: String = sqlx::query("SELECT title FROM recordings WHERE id = ?1")
        .bind(RECORDING)
        .fetch_one(library.db.pool())
        .await
        .unwrap()
        .get(0);
    assert_eq!(title, "Local");
    assert_eq!(library.count("transcript_revisions").await, 0);
    assert_eq!(library.count("speakers").await, 0);
    assert_eq!(library.count("recording_tags").await, 0);
}

#[tokio::test]
async fn an_existing_recording_fails_the_whole_restore_under_the_fail_policy() {
    let dir = tempfile::tempdir().unwrap();
    let archive = full_archive(dir.path(), "full.recording-archive");
    let library = Library::open(&dir.path().join("library")).await;
    library.insert_recording(RECORDING, None).await;

    let error = library
        .restorer()
        .restore(
            &archive,
            None,
            &RestoreOptions {
                mode: RestoreMode::Merge,
                on_conflict: ConflictPolicy::Fail,
                restore_tombstoned: false,
            },
        )
        .await
        .unwrap_err();

    let ExportError::RestoreConflict(detail) = &error else {
        panic!("expected a restore conflict, got {error:?}");
    };
    assert!(detail.contains(RECORDING), "unexpected detail {detail}");

    // The refusal happens before the transaction opens, so not one row of the
    // archive reached the library.
    assert_eq!(library.count("recordings").await, 1);
    assert_eq!(library.count("sources").await, 0);
    assert_eq!(library.count("people").await, 0);
    assert_eq!(library.count("media_assets").await, 0);
}

#[tokio::test]
async fn a_locally_deleted_recording_is_not_resurrected_unless_the_user_asks() {
    let dir = tempfile::tempdir().unwrap();
    let archive = full_archive(dir.path(), "full.recording-archive");
    let library = Library::open(&dir.path().join("library")).await;
    sqlx::query(
        "INSERT INTO tombstones (id, entity_type, entity_id, reason, created_at)
         VALUES ('t1','recording',?1,'user_deleted',1)",
    )
    .bind(RECORDING)
    .execute(library.db.pool())
    .await
    .unwrap();

    let guarded = library
        .restorer()
        .restore(&archive, None, &RestoreOptions::default())
        .await
        .unwrap();
    assert_eq!(guarded.recordings_restored, 0);
    assert_eq!(guarded.conflicts.len(), 1);
    assert_eq!(guarded.conflicts[0].reason, "deleted locally");
    assert_eq!(library.count("recordings").await, 0);

    let asked = library
        .restorer()
        .restore(
            &archive,
            None,
            &RestoreOptions {
                mode: RestoreMode::Merge,
                on_conflict: ConflictPolicy::Skip,
                restore_tombstoned: true,
            },
        )
        .await
        .unwrap();
    assert_eq!(asked.recordings_restored, 1);
    assert_eq!(asked.conflicts, Vec::new());
    assert_eq!(library.count("recordings").await, 1);
}

#[tokio::test]
async fn a_recording_deleted_in_place_is_treated_as_deleted_not_as_present() {
    let dir = tempfile::tempdir().unwrap();
    let archive = full_archive(dir.path(), "full.recording-archive");
    let library = Library::open(&dir.path().join("library")).await;
    library.insert_recording(RECORDING, Some(99)).await;

    let report = library
        .restorer()
        .restore(&archive, None, &RestoreOptions::default())
        .await
        .unwrap();

    assert_eq!(report.conflicts.len(), 1);
    assert_eq!(report.conflicts[0].reason, "deleted locally");
    assert_eq!(report.recordings_restored, 0);
}

#[tokio::test]
async fn a_source_item_naming_a_source_the_archive_does_not_carry_is_reported() {
    let dir = tempfile::tempdir().unwrap();
    let (media, digest) = media_item(dir.path(), b"RIFF....WAVE orphan");
    let mut record = record_doc(RECORDING, vec![asset_row(&digest, 19)]);
    record.source_items.push(SourceItemRow {
        id: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa".to_owned(),
        source_id: "a-source-nobody-archived".to_owned(),
        external_id: None,
        import_status: "imported".to_owned(),
        last_seen_at: None,
        remote_version: None,
        remote_state: "unknown".to_owned(),
        tombstone_reason: None,
        created_at: 9,
    });
    let archive = build_archive(
        dir.path(),
        "orphan-source.recording-archive",
        vec![
            media,
            document(&format!("records/{RECORDING}.json"), RECORD_SCHEMA, &record),
        ],
        &Protection::Plaintext,
    );
    let library = Library::open(&dir.path().join("library")).await;

    let report = library
        .restorer()
        .restore(&archive, None, &RestoreOptions::default())
        .await
        .unwrap();

    // The recording still lands; only the association that cannot be satisfied
    // without inventing a source row is dropped, and it is reported.
    assert_eq!(report.recordings_restored, 1);
    assert_eq!(report.conflicts.len(), 1);
    assert!(report.conflicts[0]
        .reason
        .contains("a-source-nobody-archived"));
    assert_eq!(library.count("source_items").await, 0);
}

#[tokio::test]
async fn a_source_item_naming_a_source_the_library_already_has_is_restored() {
    let dir = tempfile::tempdir().unwrap();
    let (media, digest) = media_item(dir.path(), b"RIFF....WAVE known");
    let mut record = record_doc(RECORDING, vec![asset_row(&digest, 18)]);
    record.source_items.push(SourceItemRow {
        id: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa".to_owned(),
        source_id: "already-here".to_owned(),
        external_id: None,
        import_status: "imported".to_owned(),
        last_seen_at: None,
        remote_version: None,
        remote_state: "unknown".to_owned(),
        tombstone_reason: None,
        created_at: 9,
    });
    let archive = build_archive(
        dir.path(),
        "known-source.recording-archive",
        vec![
            media,
            document(&format!("records/{RECORDING}.json"), RECORD_SCHEMA, &record),
        ],
        &Protection::Plaintext,
    );
    let library = Library::open(&dir.path().join("library")).await;
    sqlx::query(
        "INSERT INTO sources (id, source_kind, account_scope, display_name, status, created_at)
         VALUES ('already-here','local_file','local','Files','ready',1)",
    )
    .execute(library.db.pool())
    .await
    .unwrap();

    let report = library
        .restorer()
        .restore(&archive, None, &RestoreOptions::default())
        .await
        .unwrap();

    assert_eq!(report.conflicts, Vec::new());
    assert_eq!(library.count("source_items").await, 1);
}

#[tokio::test]
async fn tool_definitions_are_reported_when_the_build_has_no_tool_table() {
    let dir = tempfile::tempdir().unwrap();
    let archive = build_archive(
        dir.path(),
        "tools.recording-archive",
        vec![document(
            "tools/action-items-1.json",
            TOOL_SCHEMA,
            &tool_doc("action-items"),
        )],
        &Protection::Plaintext,
    );
    let library = Library::open(&dir.path().join("library")).await;
    sqlx::query("DROP TABLE transcript_tools")
        .execute(library.db.pool())
        .await
        .unwrap();

    let report = library
        .restorer()
        .restore(&archive, None, &RestoreOptions::default())
        .await
        .unwrap();

    assert_eq!(report.tools_restored, 0);
    assert_eq!(report.conflicts.len(), 1);
    assert_eq!(
        report.conflicts[0].reason,
        "this build has no transcript tool table"
    );
    assert_eq!(report.conflicts[0].entry_path, "tools/action-items.json");
}

#[tokio::test]
async fn a_speaker_naming_a_person_the_archive_does_not_carry_keeps_its_label() {
    let dir = tempfile::tempdir().unwrap();
    let (media, digest) = media_item(dir.path(), b"RIFF....WAVE unlinked");
    let archive = build_archive(
        dir.path(),
        "unlinked.recording-archive",
        vec![
            media,
            document(
                &format!("records/{RECORDING}.json"),
                RECORD_SCHEMA,
                &record_doc(RECORDING, vec![asset_row(&digest, 21)]),
            ),
            document(
                &format!("annotations/{RECORDING}.json"),
                ANNOTATION_SCHEMA,
                &annotation_doc(
                    RECORDING,
                    vec![speaker_row(
                        "cccccccc-cccc-4ccc-8ccc-cccccccccccc",
                        Some("a-person-who-did-not-travel"),
                    )],
                ),
            ),
        ],
        &Protection::Plaintext,
    );
    let library = Library::open(&dir.path().join("library")).await;

    library
        .restorer()
        .restore(&archive, None, &RestoreOptions::default())
        .await
        .unwrap();

    let row = sqlx::query("SELECT name, person_id FROM speakers")
        .fetch_one(library.db.pool())
        .await
        .unwrap();
    // The label survives; the link that cannot be resolved is dropped rather
    // than restored as a dangling reference.
    assert_eq!(
        row.get::<Option<String>, _>("name").as_deref(),
        Some("Dana")
    );
    assert_eq!(row.get::<Option<String>, _>("person_id"), None);
}

// ---------------------------------------------------------------------------
// Refusals that must leave nothing behind
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_record_referencing_media_the_archive_does_not_carry_leaves_the_library_untouched() {
    let dir = tempfile::tempdir().unwrap();
    // The record names a blob digest no media entry provides.
    let record = record_doc(RECORDING, vec![asset_row(&"c".repeat(64), 10)]);
    let archive = build_archive(
        dir.path(),
        "dangling.recording-archive",
        vec![
            document(
                "sources/source-1.json",
                SOURCE_SCHEMA,
                &source_doc("source-1"),
            ),
            document(&format!("records/{RECORDING}.json"), RECORD_SCHEMA, &record),
        ],
        &Protection::Plaintext,
    );
    let library = Library::open(&dir.path().join("library")).await;

    let error = library
        .restorer()
        .restore(&archive, None, &RestoreOptions::default())
        .await
        .unwrap_err();

    let ExportError::ArchiveRejected(ArchiveRejection::Malformed(detail)) = &error else {
        panic!("expected a malformed rejection, got {error:?}");
    };
    assert!(detail.contains("does not carry"), "unexpected {detail}");

    // The transaction never committed: not even the source that precedes the
    // failing record in insertion order survives.
    assert_eq!(library.count("recordings").await, 0);
    assert_eq!(library.count("sources").await, 0);
    assert_eq!(library.count("media_assets").await, 0);
}

#[tokio::test]
async fn a_document_that_is_not_valid_metadata_fails_before_the_library_is_touched() {
    let dir = tempfile::tempdir().unwrap();
    for (name, path, schema, bytes) in [
        (
            "record",
            format!("records/{RECORDING}.json"),
            RECORD_SCHEMA,
            br#"{"schema_version":"archive-record/1"}"#.to_vec(),
        ),
        (
            "annotation",
            format!("annotations/{RECORDING}.json"),
            ANNOTATION_SCHEMA,
            b"not json at all".to_vec(),
        ),
        (
            "transcript",
            format!("transcripts/{REVISION}.json"),
            TRANSCRIPT_SCHEMA,
            br#"{"unexpected":true}"#.to_vec(),
        ),
        (
            "output",
            "outputs/o.json".to_owned(),
            OUTPUT_SCHEMA,
            b"[]".to_vec(),
        ),
        (
            "source",
            "sources/s.json".to_owned(),
            SOURCE_SCHEMA,
            b"3".to_vec(),
        ),
        (
            "tool",
            "tools/t.json".to_owned(),
            TOOL_SCHEMA,
            b"null".to_vec(),
        ),
        (
            "person",
            "people/p.json".to_owned(),
            PERSON_SCHEMA,
            br#"{"schema_version":"archive-person/1","id":"p"}"#.to_vec(),
        ),
    ] {
        let work = dir.path().join(name);
        std::fs::create_dir_all(&work).unwrap();
        let archive = build_archive(
            &work,
            "broken.recording-archive",
            vec![Item::Document {
                path,
                schema,
                bytes,
            }],
            &Protection::Plaintext,
        );
        let library = Library::open(&work.join("library")).await;

        let error = library
            .restorer()
            .restore(&archive, None, &RestoreOptions::default())
            .await
            .unwrap_err();
        let ExportError::ArchiveRejected(ArchiveRejection::Malformed(detail)) = &error else {
            panic!("{name}: expected a malformed rejection, got {error:?}");
        };
        assert!(detail.contains(name), "{name}: unexpected detail {detail}");
        assert_eq!(library.count("recordings").await, 0);
        assert_eq!(library.count("sources").await, 0);
        assert_eq!(library.count("people").await, 0);
    }
}

#[tokio::test]
async fn a_blob_already_present_under_a_different_content_fails_verification() {
    let dir = tempfile::tempdir().unwrap();
    let bytes = b"RIFF....WAVE the real bytes".to_vec();
    let (media, digest) = media_item(dir.path(), &bytes);
    let archive = build_archive(
        dir.path(),
        "collide.recording-archive",
        vec![
            media,
            document(
                &format!("records/{RECORDING}.json"),
                RECORD_SCHEMA,
                &record_doc(RECORDING, vec![asset_row(&digest, bytes.len() as u64)]),
            ),
        ],
        &Protection::Plaintext,
    );
    let library = Library::open(&dir.path().join("library")).await;

    // Plant impostor bytes at the content address the restore will look for.
    let relative = format!("media/sha256/{}/{digest}.wav", &digest[..2]);
    let planted = library.media.absolute(&relative).unwrap();
    std::fs::create_dir_all(planted.parent().unwrap()).unwrap();
    std::fs::write(&planted, b"not the bytes this digest promises").unwrap();

    let error = library
        .restorer()
        .restore(&archive, None, &RestoreOptions::default())
        .await
        .unwrap_err();

    assert!(
        matches!(error, ExportError::Persistence(_)),
        "expected a persistence failure, got {error:?}"
    );
    assert_eq!(library.count("recordings").await, 0);
}

#[tokio::test]
async fn a_library_whose_staging_area_cannot_be_created_fails_without_writing() {
    let dir = tempfile::tempdir().unwrap();
    let archive = full_archive(dir.path(), "full.recording-archive");
    let root = dir.path().join("library");
    let library = Library::open(&root).await;
    // A plain file where the staging directory belongs: `create_dir_all`
    // cannot proceed, and the restore must stop there.
    std::fs::write(root.join("temp"), b"not a directory").unwrap();

    let error = library
        .restorer()
        .restore(&archive, None, &RestoreOptions::default())
        .await
        .unwrap_err();

    assert!(
        matches!(error, ExportError::Io(_)),
        "expected a storage failure, got {error:?}"
    );
    assert_eq!(library.count("recordings").await, 0);
    assert_eq!(library.count("media_assets").await, 0);
}

// ---------------------------------------------------------------------------
// The biometric tier (VP-078)
// ---------------------------------------------------------------------------

fn voice_archive(work: &Path, name: &str, items: Vec<Item>) -> PathBuf {
    build_archive(work, name, items, &encrypted())
}

#[tokio::test]
async fn voice_profiles_rearm_the_consent_gate_and_restore_with_their_roster() {
    let dir = tempfile::tempdir().unwrap();
    let (media, digest) = media_item(dir.path(), b"RIFF....WAVE voices");
    let archive = voice_archive(
        dir.path(),
        "voices.recording-archive",
        vec![
            document(
                &format!("people/{PERSON}.json"),
                PERSON_SCHEMA,
                &person_doc(PERSON),
            ),
            media,
            document(
                &format!("records/{RECORDING}.json"),
                RECORD_SCHEMA,
                &record_doc(RECORDING, vec![asset_row(&digest, 19)]),
            ),
            Item::Voice {
                path: VoiceProfileDocument::entry_path(PERSON),
                bytes: serde_json::to_vec(&voice_doc(PERSON, Some(RECORDING))).unwrap(),
            },
        ],
    );
    let library = Library::open(&dir.path().join("library")).await;

    let report = library
        .restorer()
        .restore(&archive, Some(PASSWORD), &RestoreOptions::default())
        .await
        .unwrap();

    assert_eq!(report.voice_profiles_restored, 1);
    assert!(report.voice_consent_required);
    assert_eq!(report.conflicts, Vec::new());
    assert_eq!(library.count("person_voice_enrollments").await, 1);
    assert_eq!(library.count("voice_exemplars").await, 1);

    // The exemplar's provenance resolved, because the recording travelled too.
    let row = sqlx::query("SELECT recording_id, cluster_key FROM voice_exemplars")
        .fetch_one(library.db.pool())
        .await
        .unwrap();
    assert_eq!(
        row.get::<Option<String>, _>("recording_id").as_deref(),
        Some(RECORDING)
    );
    assert_eq!(
        row.get::<Option<String>, _>("cluster_key").as_deref(),
        Some("cluster-1")
    );
}

#[tokio::test]
async fn an_exemplar_naming_a_recording_this_library_lacks_drops_the_provenance() {
    let dir = tempfile::tempdir().unwrap();
    let archive = voice_archive(
        dir.path(),
        "orphan-provenance.recording-archive",
        vec![
            document(
                &format!("people/{PERSON}.json"),
                PERSON_SCHEMA,
                &person_doc(PERSON),
            ),
            Item::Voice {
                path: VoiceProfileDocument::entry_path(PERSON),
                bytes: serde_json::to_vec(&voice_doc(PERSON, Some("a-recording-far-away")))
                    .unwrap(),
            },
        ],
    );
    let library = Library::open(&dir.path().join("library")).await;

    let report = library
        .restorer()
        .restore(&archive, Some(PASSWORD), &RestoreOptions::default())
        .await
        .unwrap();

    assert_eq!(report.voice_profiles_restored, 1);
    let row = sqlx::query("SELECT recording_id, cluster_key FROM voice_exemplars")
        .fetch_one(library.db.pool())
        .await
        .unwrap();
    assert_eq!(row.get::<Option<String>, _>("recording_id"), None);
    // The cluster key is provenance about that run; it goes with it.
    assert_eq!(row.get::<Option<String>, _>("cluster_key"), None);
}

#[tokio::test]
async fn a_voice_profile_without_a_roster_entry_is_reported_not_inserted() {
    let dir = tempfile::tempdir().unwrap();
    let archive = voice_archive(
        dir.path(),
        "no-roster.recording-archive",
        vec![Item::Voice {
            path: VoiceProfileDocument::entry_path(PERSON),
            bytes: serde_json::to_vec(&voice_doc(PERSON, None)).unwrap(),
        }],
    );
    let library = Library::open(&dir.path().join("library")).await;

    let report = library
        .restorer()
        .restore(&archive, Some(PASSWORD), &RestoreOptions::default())
        .await
        .unwrap();

    assert_eq!(report.voice_profiles_restored, 0);
    assert!(report.voice_consent_required);
    assert_eq!(report.conflicts.len(), 1);
    assert_eq!(
        report.conflicts[0].reason,
        "the archive carries no roster entry for this enrolment"
    );
    assert_eq!(library.count("person_voice_enrollments").await, 0);
    assert_eq!(library.count("voice_exemplars").await, 0);
}

#[tokio::test]
async fn a_person_already_enrolled_in_the_target_library_keeps_their_own_exemplars() {
    let dir = tempfile::tempdir().unwrap();
    let archive = voice_archive(
        dir.path(),
        "already-enrolled.recording-archive",
        vec![
            document(
                &format!("people/{PERSON}.json"),
                PERSON_SCHEMA,
                &person_doc(PERSON),
            ),
            Item::Voice {
                path: VoiceProfileDocument::entry_path(PERSON),
                bytes: serde_json::to_vec(&voice_doc(PERSON, None)).unwrap(),
            },
        ],
    );
    let library = Library::open(&dir.path().join("library")).await;
    sqlx::query(
        "INSERT INTO people (id, display_name, created_at, updated_at, status)
         VALUES (?1, 'Dana Scully', 1, 1, 'active')",
    )
    .bind(PERSON)
    .execute(library.db.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO person_voice_enrollments
             (person_id, consent_version, retention_months, status, created_at)
         VALUES (?1, 'voice-consent/1', 12, 'active', 1)",
    )
    .bind(PERSON)
    .execute(library.db.pool())
    .await
    .unwrap();

    let report = library
        .restorer()
        .restore(&archive, Some(PASSWORD), &RestoreOptions::default())
        .await
        .unwrap();

    assert_eq!(report.voice_profiles_restored, 0);
    assert_eq!(report.conflicts.len(), 1);
    assert_eq!(
        report.conflicts[0].reason,
        "this person is already enrolled in the target library"
    );
    // The existing enrolment kept its own (empty) exemplar set.
    assert_eq!(library.count("voice_exemplars").await, 0);
}

#[tokio::test]
async fn an_invalid_voice_profile_document_fails_before_the_first_insert() {
    let dir = tempfile::tempdir().unwrap();
    let mut profile = voice_doc(PERSON, None);
    profile.enrollment.retention_months = 0;
    let archive = voice_archive(
        dir.path(),
        "bad-retention.recording-archive",
        vec![
            document(
                &format!("people/{PERSON}.json"),
                PERSON_SCHEMA,
                &person_doc(PERSON),
            ),
            Item::Voice {
                path: VoiceProfileDocument::entry_path(PERSON),
                bytes: serde_json::to_vec(&profile).unwrap(),
            },
        ],
    );
    let library = Library::open(&dir.path().join("library")).await;

    let error = library
        .restorer()
        .restore(&archive, Some(PASSWORD), &RestoreOptions::default())
        .await
        .unwrap_err();

    let ExportError::ArchiveRejected(ArchiveRejection::Malformed(detail)) = &error else {
        panic!("expected a malformed rejection, got {error:?}");
    };
    assert!(detail.contains("voice-profile"), "unexpected {detail}");
    assert_eq!(library.count("people").await, 0);
    assert_eq!(library.count("person_voice_enrollments").await, 0);
}

#[tokio::test]
async fn an_exemplar_with_a_malformed_sealed_value_fails_the_restore() {
    let dir = tempfile::tempdir().unwrap();
    let mut profile = voice_doc(PERSON, None);
    profile.exemplars[0].embedding_nonce_b64 = "not base64 !!".to_owned();
    let archive = voice_archive(
        dir.path(),
        "bad-nonce.recording-archive",
        vec![
            document(
                &format!("people/{PERSON}.json"),
                PERSON_SCHEMA,
                &person_doc(PERSON),
            ),
            Item::Voice {
                path: VoiceProfileDocument::entry_path(PERSON),
                bytes: serde_json::to_vec(&profile).unwrap(),
            },
        ],
    );
    let library = Library::open(&dir.path().join("library")).await;

    let error = library
        .restorer()
        .restore(&archive, Some(PASSWORD), &RestoreOptions::default())
        .await
        .unwrap_err();

    let ExportError::ArchiveRejected(ArchiveRejection::Malformed(detail)) = &error else {
        panic!("expected a malformed rejection, got {error:?}");
    };
    assert!(detail.contains("sealed value"), "unexpected {detail}");
    assert_eq!(library.count("people").await, 0);
    assert_eq!(library.count("voice_exemplars").await, 0);
}

#[tokio::test]
async fn voice_profiles_cannot_be_staged_into_a_plaintext_archive() {
    assert!(matches!(
        VoiceProfilePermit::for_protection(&Protection::Plaintext),
        Err(ExportError::VoiceProfilesRequireEncryption)
    ));
    assert!(VoiceProfilePermit::for_protection(&encrypted()).is_ok());

    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("staging")).unwrap();
    std::fs::create_dir_all(dir.path().join("out")).unwrap();
    let mut builder =
        ArchiveBuilder::new(&dir.path().join("staging"), Timestamp::from_millis(1)).unwrap();
    let permit = VoiceProfilePermit::for_protection(&encrypted()).unwrap();

    // A voice document outside the namespace is refused even with a permit.
    let bytes = serde_json::to_vec(&voice_doc(PERSON, None)).unwrap();
    let error = builder
        .add_voice_document("people/not-here.json", &bytes, permit)
        .unwrap_err();
    assert!(matches!(
        error,
        ExportError::ArchiveRejected(ArchiveRejection::PathRejected { .. })
    ));

    // A builder that holds biometric material refuses a plaintext emission,
    // even though the permit was obtained honestly.
    builder
        .add_voice_document(&VoiceProfileDocument::entry_path(PERSON), &bytes, permit)
        .unwrap();
    let destination = ExportDestination::new(dir.path().join("out")).unwrap();
    assert!(matches!(
        builder
            .finish(
                &destination,
                "plain.recording-archive",
                &Protection::Plaintext
            )
            .unwrap_err(),
        ExportError::VoiceProfilesRequireEncryption
    ));
    assert_eq!(
        std::fs::read_dir(dir.path().join("out")).unwrap().count(),
        0
    );
}

// ---------------------------------------------------------------------------
// Dry run
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_dry_run_reports_conflicts_and_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let archive = full_archive(dir.path(), "full.recording-archive");
    let library = Library::open(&dir.path().join("library")).await;

    let clean = library.restorer().dry_run(&archive, None).await.unwrap();
    assert!(clean.target_is_empty);
    assert_eq!(clean.conflicts, Vec::new());
    assert!(clean.inspection.payloads_verified);
    assert!(!clean.inspection.encrypted);

    library.insert_recording(RECORDING, None).await;
    let conflicted = library.restorer().dry_run(&archive, None).await.unwrap();
    assert!(!conflicted.target_is_empty);
    assert_eq!(conflicted.conflicts.len(), 1);
    assert_eq!(conflicted.conflicts[0].reason, "already in this library");

    // Still nothing extracted and nothing inserted beyond the local row.
    assert_eq!(library.count("recordings").await, 1);
    assert_eq!(library.count("media_assets").await, 0);
    assert!(!library.root.join("temp").join("restore-dry-run").exists());
}

#[tokio::test]
async fn a_dry_run_of_a_damaged_archive_reports_the_damage() {
    let dir = tempfile::tempdir().unwrap();
    let archive = full_archive(dir.path(), "full.recording-archive");
    let mut bytes = std::fs::read(&archive).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 0xff;
    std::fs::write(&archive, &bytes).unwrap();

    let library = Library::open(&dir.path().join("library")).await;
    let error = library
        .restorer()
        .dry_run(&archive, None)
        .await
        .unwrap_err();
    assert!(matches!(error, ExportError::ArchiveRejected(_)));
}

// ---------------------------------------------------------------------------
// The scanner assumption the tool fixture above leans on
// ---------------------------------------------------------------------------

#[test]
fn a_bare_hex_run_in_an_undocumented_digest_field_is_a_finding() {
    let documented = serde_json::to_vec(&serde_json::json!({
        "sha256": "a".repeat(64),
        "input_sha256": "b".repeat(64),
        "integrity_digest": "c".repeat(64),
        "model_sha256": "d".repeat(64),
    }))
    .unwrap();
    assert_eq!(
        ArchiveSecretScanner::findings("records/a.json", &documented),
        Vec::new()
    );

    let undocumented =
        serde_json::to_vec(&serde_json::json!({ "definition_sha256": "e".repeat(64) })).unwrap();
    let findings = ArchiveSecretScanner::findings("tools/a.json", &undocumented);
    assert_eq!(findings.len(), 1, "unexpected findings {findings:?}");
    assert_eq!(findings[0].field, "/definition_sha256");

    // Which is why the fixture uses a non-hex 64-character value.
    let fixture = serde_json::to_vec(&tool_doc("action-items")).unwrap();
    assert_eq!(
        ArchiveSecretScanner::findings("tools/a.json", &fixture),
        Vec::new()
    );
}
