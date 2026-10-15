//! The four spec 15.2 export commands against a real library: what they
//! produce, what they refuse per format, and what they leave on disk when the
//! destination or the archive is unusable.
//!
//! The seeded library below is deliberately awkward — a title full of
//! separators, one confirmed and one unconfirmed speaker, a revision with real
//! times and a revision with none — because the branches worth covering are
//! the ones a tidy fixture never reaches.

use std::path::{Path, PathBuf};

use plaudon_domain::ids::{MediaAssetId, RecordingId, TranscriptRevisionId};
use plaudon_domain::time::Timestamp;
use plaudon_export::archive::restore::{ConflictPolicy, RestoreMode, RestoreOptions};
use plaudon_export::archive::snapshot::{extension_of, LibrarySnapshot};
use plaudon_export::bundle::{BundleLoader, ExportNotices};
use plaudon_export::destination::{build_file_name, ExportDestination};
use plaudon_export::error::{ArchiveRejection, CaptionRefusal, ExportError};
use plaudon_export::service::{
    BackupRequest, ExportFormat, ExportMediaRequest, ExportRecordingRequest, ExportService,
    InspectArchiveRequest, RestoreArchiveRequest,
};
use plaudon_persistence::archive::MediaArchive;
use plaudon_persistence::migrations::MigrationRunner;
use plaudon_persistence::Database;
use sha2::{Digest as _, Sha256};
use sqlx::Row as _;

const NOW: Timestamp = Timestamp::from_millis(1_700_000_300_000);
const PASSWORD: &str = "correct horse battery staple";

const TIMED: &str = "11111111-1111-4111-8111-111111111111";
const TEXT_ONLY: &str = "22222222-2222-4222-8222-222222222222";
const TIMED_REVISION: &str = "33333333-3333-4333-8333-333333333333";
const TEXT_REVISION: &str = "44444444-4444-4444-8444-444444444444";
const ASSET: &str = "55555555-5555-4555-8555-555555555555";
const PERSON: &str = "66666666-6666-4666-8666-666666666666";

const BLOB: &[u8] = b"not a container we can sanitize, just bytes";

#[derive(Debug)]
struct Library {
    root: PathBuf,
    db: Database,
    media: MediaArchive,
}

impl Library {
    async fn empty(root: &Path) -> Self {
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

    fn service(&self) -> ExportService {
        ExportService::new(self.db.clone(), self.media.clone(), self.root.clone())
    }

    async fn count(&self, table: &str) -> i64 {
        let sql = format!("SELECT COUNT(*) FROM {table}");
        sqlx::query(sqlx::AssertSqlSafe(sql))
            .fetch_one(self.db.pool())
            .await
            .unwrap()
            .get::<i64, _>(0)
    }
}

/// Seed one library with two recordings: one with real alignment and audio,
/// one with a text-only revision and no media.
async fn seeded(root: &Path) -> Library {
    let library = Library::empty(root).await;
    let pool = library.db.pool();

    let digest = hex::encode(Sha256::digest(BLOB));
    let relative = format!("media/sha256/{}/{digest}.bin", &digest[..2]);
    let absolute = library.media.absolute(&relative).unwrap();
    std::fs::create_dir_all(absolute.parent().unwrap()).unwrap();
    std::fs::write(&absolute, BLOB).unwrap();

    sqlx::query(
        "INSERT INTO media_assets
             (id, sha256, byte_length, relative_path, mime, codec, sample_rate, channels,
              duration_ms, quality_class, verification_state, verified_at, created_at)
         VALUES (?1, ?2, ?3, ?4, 'audio/wav', 'pcm_s16le', 16000, 1, 2000,
                 'compressed_device_transfer', 'verified', 1, 1)",
    )
    .bind(ASSET)
    .bind(&digest)
    .bind(BLOB.len() as i64)
    .bind(&relative)
    .execute(pool)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO recordings
             (id, title, captured_at, duration_ms, preferred_media_asset_id, annotation, created_at)
         VALUES (?1, 'Weekly sync: budget/plans', 100, 2000, ?2, 'Dana agreed the budget', 10)",
    )
    .bind(TIMED)
    .bind(ASSET)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO recordings (id, title, created_at) VALUES (?1, 'Notes only', 20)")
        .bind(TEXT_ONLY)
        .execute(pool)
        .await
        .unwrap();

    sqlx::query(
        "INSERT INTO recording_media (recording_id, media_asset_id, quality_class, obtained_at)
         VALUES (?1, ?2, 'compressed_device_transfer', 11)",
    )
    .bind(TIMED)
    .bind(ASSET)
    .execute(pool)
    .await
    .unwrap();
    for tag in ["budget", "planning"] {
        sqlx::query("INSERT INTO recording_tags (recording_id, tag) VALUES (?1, ?2)")
            .bind(TIMED)
            .bind(tag)
            .execute(pool)
            .await
            .unwrap();
    }

    sqlx::query(
        "INSERT INTO sources (id, source_kind, account_scope, display_name, status, created_at)
         VALUES ('source-1','local_file','local','Files','ready',1)",
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO source_items
             (id, source_id, external_id, recording_id, import_status, last_seen_at,
              remote_version, remote_state, created_at)
         VALUES ('item-1','source-1','external-1',?1,'imported',12,'v1','present',9)",
    )
    .bind(TIMED)
    .execute(pool)
    .await
    .unwrap();

    // One confirmed identity and one inferred cluster: the renderers must treat
    // them differently.
    sqlx::query(
        "INSERT INTO speakers (id, recording_id, label, name, provenance, user_confirmed)
         VALUES ('speaker-1', ?1, 'Speaker 1', 'Dana', 'user_confirmed', 1)",
    )
    .bind(TIMED)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO speakers (id, recording_id, label, name, provenance, user_confirmed)
         VALUES ('speaker-2', ?1, 'Speaker 2', 'Maybe Alex', 'inferred', 0)",
    )
    .bind(TIMED)
    .execute(pool)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO transcript_revisions
             (id, recording_id, origin, input_sha256, engine, model_id, model_version, locale,
              alignment_origin, status, normalization_profile, chunking_strategy, duration_ms,
              created_at, engine_backend, engine_params_json)
         VALUES (?1, ?2, 'derived_alignment', ?3, 'whisper.cpp', 'tiny', '1', 'en',
                 'derived', 'complete', 'default', 'none', 2000, 30, 'cpu', '{\"beam\":1}')",
    )
    .bind(TIMED_REVISION)
    .bind(TIMED)
    .bind(&digest)
    .execute(pool)
    .await
    .unwrap();

    // Overlapping segments, so the caption builder's clamping runs.
    for (id, seq, start, end, speaker, text) in [
        (
            "segment-1",
            0i64,
            0i64,
            1200i64,
            Some("speaker-1"),
            "We agreed the budget.",
        ),
        (
            "segment-2",
            1,
            1000,
            2000,
            Some("speaker-2"),
            "I will send the plan.",
        ),
        ("segment-3", 2, 2000, 2000, None, "Thanks everyone."),
    ] {
        sqlx::query(
            "INSERT INTO transcript_segments
                 (id, transcript_revision_id, seq_index, start_ms, end_ms, speaker_id, text,
                  confidence, confidence_scale, alignment_origin)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0.9, 'engine', 'derived')",
        )
        .bind(id)
        .bind(TIMED_REVISION)
        .bind(seq)
        .bind(start)
        .bind(end)
        .bind(speaker)
        .bind(text)
        .execute(pool)
        .await
        .unwrap();
    }
    sqlx::query(
        "INSERT INTO transcript_words
             (id, segment_id, seq_index, start_ms, end_ms, text, confidence, confidence_scale)
         VALUES ('word-1','segment-1',0,0,400,'We',0.9,'engine')",
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO transcript_annotations
             (id, transcript_revision_id, segment_index, kind, reason, start_ms, end_ms)
         VALUES ('annotation-1', ?1, 0, 'low_confidence', 'the engine reported a low score', 0, 1200)",
    )
    .bind(TIMED_REVISION)
    .execute(pool)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO ai_outputs
             (id, recording_id, output_type, transcript_revision_id, template_version, model_id,
              prompt_version, schema_version, body_json, status, created_at)
         VALUES ('output-1', ?1, 'meeting_summary', ?2, '1', 'local', '1', 'meeting-summary/1',
                 '{\"summary\":\"We agreed the budget.\"}', 'complete', 40)",
    )
    .bind(TIMED)
    .bind(TIMED_REVISION)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO output_claims
             (id, output_id, item_path, kind, category, claim_text, state, entailment)
         VALUES ('claim-1','output-1','/summary','summary_statement','general',
                 'We agreed the budget.','source_linked','supported')",
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO output_evidence
             (id, output_id, claim_id, position, item_path, cited_revision_id, seq_index,
              char_start, char_end, quote, verification_status, matched_text, alignment_origin,
              seek_ms)
         VALUES ('evidence-1','output-1','claim-1',0,'/summary', ?1, 0, 0, 21,
                 'We agreed the budget.','verified','We agreed the budget.','derived',0)",
    )
    .bind(TIMED_REVISION)
    .execute(pool)
    .await
    .unwrap();

    // The second recording: a revision the engine returned without times.
    sqlx::query(
        "INSERT INTO transcript_revisions
             (id, recording_id, origin, input_sha256, engine, model_id, alignment_origin, status,
              normalization_profile, chunking_strategy, created_at)
         VALUES (?1, ?2, 'machine', ?3, 'whisper.cpp', 'tiny', 'none', 'complete',
                 'default', 'none', 50)",
    )
    .bind(TEXT_REVISION)
    .bind(TEXT_ONLY)
    .bind("a".repeat(64))
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO transcript_segments
             (id, transcript_revision_id, seq_index, text, alignment_origin)
         VALUES ('segment-9', ?1, 0, 'No times exist for this revision.', 'none')",
    )
    .bind(TEXT_REVISION)
    .execute(pool)
    .await
    .unwrap();

    library
}

async fn seed_voice_profile(library: &Library) {
    let pool = library.db.pool();
    sqlx::query(
        "INSERT INTO people (id, display_name, aliases_json, created_at, updated_at, status)
         VALUES (?1, 'Dana Scully', '[\"Dana\"]', 1, 1, 'active')",
    )
    .bind(PERSON)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO person_voice_enrollments
             (person_id, consent_version, consent_note, retention_months, status, created_at)
         VALUES (?1, 'voice-consent/1', 'asked in the meeting', 6, 'active', 2)",
    )
    .bind(PERSON)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO voice_exemplars
             (id, person_id, recording_id, speech_ms, model_id, model_version, model_sha256,
              embedding_dim, embedding_cipher, embedding_nonce, created_at)
         VALUES ('exemplar-1', ?1, ?2, 2000, 'ecapa', '1', ?3, 192, ?4, ?5, 3)",
    )
    .bind(PERSON)
    .bind(TIMED)
    .bind("b".repeat(64))
    .bind(vec![7u8; 64])
    .bind(vec![9u8; 24])
    .execute(pool)
    .await
    .unwrap();
}

fn out_dir(dir: &Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::create_dir_all(&path).unwrap();
    path
}

fn recording(id: &str) -> RecordingId {
    id.parse().unwrap()
}

// ---------------------------------------------------------------------------
// export_recording
// ---------------------------------------------------------------------------

#[tokio::test]
async fn every_format_lands_for_a_revision_that_has_real_times() {
    let dir = tempfile::tempdir().unwrap();
    let library = seeded(&dir.path().join("library")).await;
    let destination = out_dir(dir.path(), "exports");

    let response = library
        .service()
        .export_recording(
            &ExportRecordingRequest {
                recording_id: recording(TIMED),
                transcript_revision_id: None,
                formats: ExportFormat::ALL.to_vec(),
                destination: destination.clone(),
            },
            NOW,
        )
        .await
        .unwrap();

    assert_eq!(response.files.len(), 5);
    assert_eq!(response.refusals, Vec::new());
    assert_eq!(
        response.transcript_revision_id,
        Some(TIMED_REVISION.parse::<TranscriptRevisionId>().unwrap())
    );
    let resolved = destination.canonicalize().unwrap();
    for file in &response.files {
        assert!(file.path.starts_with(&resolved), "{:?}", file.path);
        assert_eq!(
            std::fs::metadata(&file.path).unwrap().len(),
            file.byte_length
        );
        // The title's separators never reach the filesystem.
        let name = file
            .path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert!(!name.contains('/'), "{name}");
        assert!(!name.contains(':'), "{name}");
        assert!(
            name.ends_with(&format!(".{}", file.format.extension())),
            "{name}"
        );
    }

    let read = |format: ExportFormat| -> String {
        let file = response
            .files
            .iter()
            .find(|file| file.format == format)
            .unwrap();
        std::fs::read_to_string(&file.path).unwrap()
    };

    let text = read(ExportFormat::Txt);
    assert!(text.contains("Weekly sync: budget/plans"));
    assert!(text.contains("not the original device WAV"));
    // An unconfirmed cluster never renders as an identity.
    assert!(!text.contains("Maybe Alex"), "{text}");

    let vtt = read(ExportFormat::Vtt);
    assert!(vtt.starts_with("WEBVTT"));
    assert!(vtt.contains("NOTE These timings come from a separate alignment pass"));
    assert!(vtt.contains("00:00:00.000 --> 00:00:01.000"));

    let srt = read(ExportFormat::Srt);
    assert!(srt.contains("[derived (approximate) alignment]"));
    assert!(srt.contains("00:00:00,000 --> 00:00:01,000"));

    let json: serde_json::Value = serde_json::from_str(&read(ExportFormat::Json)).unwrap();
    assert_eq!(json["schema_version"], "recording-export/1");

    // Every write was recorded, once per format.
    assert_eq!(library.count("export_events").await, 5);
}

#[tokio::test]
async fn captions_are_refused_per_format_when_a_revision_has_no_times() {
    let dir = tempfile::tempdir().unwrap();
    let library = seeded(&dir.path().join("library")).await;
    let destination = out_dir(dir.path(), "exports");

    let response = library
        .service()
        .export_recording(
            &ExportRecordingRequest {
                recording_id: recording(TEXT_ONLY),
                transcript_revision_id: None,
                formats: ExportFormat::ALL.to_vec(),
                destination,
            },
            NOW,
        )
        .await
        .unwrap();

    assert_eq!(response.files.len(), 3);
    assert_eq!(response.refusals.len(), 2);
    for refusal in &response.refusals {
        assert!(matches!(
            refusal.format,
            ExportFormat::Srt | ExportFormat::Vtt
        ));
        assert_eq!(refusal.code, "CAPTIONS_UNAVAILABLE");
        assert_eq!(
            refusal.explanation,
            CaptionRefusal::NoAlignment.explanation()
        );
    }
    // The refused formats were not recorded as exports.
    assert_eq!(library.count("export_events").await, 3);
}

#[tokio::test]
async fn a_recording_without_any_revision_refuses_captions_for_want_of_segments() {
    let dir = tempfile::tempdir().unwrap();
    let library = seeded(&dir.path().join("library")).await;
    let pool = library.db.pool();

    // Leave the recording with no revision at all, which is a different
    // refusal from "the revision has no times".
    sqlx::query("DELETE FROM transcript_segments WHERE transcript_revision_id = ?1")
        .bind(TEXT_REVISION)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM transcript_revisions WHERE id = ?1")
        .bind(TEXT_REVISION)
        .execute(pool)
        .await
        .unwrap();

    let response = library
        .service()
        .export_recording(
            &ExportRecordingRequest {
                recording_id: recording(TEXT_ONLY),
                transcript_revision_id: None,
                formats: vec![ExportFormat::Srt, ExportFormat::Txt],
                destination: out_dir(dir.path(), "exports"),
            },
            NOW,
        )
        .await
        .unwrap();

    assert_eq!(response.transcript_revision_id, None);
    assert_eq!(response.files.len(), 1);
    assert_eq!(response.refusals.len(), 1);
    assert_eq!(
        response.refusals[0].explanation,
        CaptionRefusal::NoSegments.explanation()
    );
    let text = std::fs::read_to_string(&response.files[0].path).unwrap();
    assert!(text.contains("No transcript revision exists yet."));
}

#[tokio::test]
async fn an_unknown_recording_or_revision_is_refused_before_anything_is_written() {
    let dir = tempfile::tempdir().unwrap();
    let library = seeded(&dir.path().join("library")).await;
    let destination = out_dir(dir.path(), "exports");

    let missing = library
        .service()
        .export_recording(
            &ExportRecordingRequest {
                recording_id: RecordingId::new(),
                transcript_revision_id: None,
                formats: vec![ExportFormat::Txt],
                destination: destination.clone(),
            },
            NOW,
        )
        .await
        .unwrap_err();
    assert!(matches!(missing, ExportError::RecordNotFound));

    let wrong_revision = library
        .service()
        .export_recording(
            &ExportRecordingRequest {
                recording_id: recording(TIMED),
                transcript_revision_id: Some(TranscriptRevisionId::new()),
                formats: vec![ExportFormat::Txt],
                destination: destination.clone(),
            },
            NOW,
        )
        .await
        .unwrap_err();
    assert!(matches!(wrong_revision, ExportError::RecordNotFound));

    assert_eq!(std::fs::read_dir(&destination).unwrap().count(), 0);
    assert_eq!(library.count("export_events").await, 0);
}

#[tokio::test]
async fn an_unusable_destination_is_refused_with_a_reason() {
    let dir = tempfile::tempdir().unwrap();
    let library = seeded(&dir.path().join("library")).await;
    let file = dir.path().join("a-file");
    std::fs::write(&file, b"x").unwrap();

    for destination in [PathBuf::new(), dir.path().join("does-not-exist"), file] {
        let error = library
            .service()
            .export_recording(
                &ExportRecordingRequest {
                    recording_id: recording(TIMED),
                    transcript_revision_id: None,
                    formats: vec![ExportFormat::Txt],
                    destination: destination.clone(),
                },
                NOW,
            )
            .await
            .unwrap_err();
        assert!(
            matches!(error, ExportError::DestinationRejected(_)),
            "{destination:?} gave {error:?}"
        );
        assert_eq!(
            error.code(),
            plaudon_domain::error::AppErrorCode::ExportDestinationRejected
        );
    }
}

// ---------------------------------------------------------------------------
// export_media
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_original_is_copied_verbatim_and_an_unsanitizable_container_is_reported() {
    let dir = tempfile::tempdir().unwrap();
    let library = seeded(&dir.path().join("library")).await;
    let destination = out_dir(dir.path(), "audio");

    let response = library
        .service()
        .export_media(
            &ExportMediaRequest {
                recording_id: recording(TIMED),
                media_asset_id: None,
                destination: destination.clone(),
                include_sanitized_derivative: true,
            },
            NOW,
        )
        .await
        .unwrap();

    assert!(response.original.is_original);
    assert_eq!(response.original.byte_length, BLOB.len() as u64);
    assert_eq!(std::fs::read(&response.original.path).unwrap(), BLOB);
    assert!(response
        .original
        .label
        .contains("not the original device WAV"));

    // These bytes are not a container we can strip without re-encoding, so the
    // derivative is refused and said so rather than silently skipped.
    assert!(response.sanitized.is_none());
    let refusal = response.sanitized_refusal.unwrap();
    assert!(
        refusal.contains("sanitized export is not available"),
        "{refusal}"
    );
    assert_eq!(library.count("export_derivatives").await, 0);
    assert_eq!(library.count("export_events").await, 1);
}

#[tokio::test]
async fn a_sanitized_derivative_is_recorded_as_its_own_artifact() {
    let dir = tempfile::tempdir().unwrap();
    let library = seeded(&dir.path().join("library")).await;

    // Replace the stored blob with a WAV carrying a metadata chunk.
    let mut wav = Vec::new();
    let data = vec![0u8; 16];
    let list = b"plaud-serial-001";
    let payload = 4 + (8 + 16) + (8 + list.len()) + (8 + data.len());
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(payload as u32).to_le_bytes());
    wav.extend_from_slice(b"WAVE");
    wav.extend_from_slice(b"fmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&[1, 0, 1, 0]);
    wav.extend_from_slice(&16_000u32.to_le_bytes());
    wav.extend_from_slice(&32_000u32.to_le_bytes());
    wav.extend_from_slice(&[2, 0, 16, 0]);
    wav.extend_from_slice(b"LIST");
    wav.extend_from_slice(&(list.len() as u32).to_le_bytes());
    wav.extend_from_slice(list);
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&(data.len() as u32).to_le_bytes());
    wav.extend_from_slice(&data);

    let digest = hex::encode(Sha256::digest(&wav));
    let relative = format!("media/sha256/{}/{digest}.wav", &digest[..2]);
    let absolute = library.media.absolute(&relative).unwrap();
    std::fs::create_dir_all(absolute.parent().unwrap()).unwrap();
    std::fs::write(&absolute, &wav).unwrap();
    sqlx::query("UPDATE media_assets SET sha256 = ?1, byte_length = ?2, relative_path = ?3")
        .bind(&digest)
        .bind(wav.len() as i64)
        .bind(&relative)
        .execute(library.db.pool())
        .await
        .unwrap();

    let destination = out_dir(dir.path(), "audio");
    let response = library
        .service()
        .export_media(
            &ExportMediaRequest {
                recording_id: recording(TIMED),
                media_asset_id: Some(ASSET.parse::<MediaAssetId>().unwrap()),
                destination,
                include_sanitized_derivative: true,
            },
            NOW,
        )
        .await
        .unwrap();

    let sanitized = response.sanitized.unwrap();
    assert!(!sanitized.is_original);
    assert_eq!(sanitized.stripped_metadata, vec!["LIST".to_owned()]);
    assert!(sanitized.byte_length < response.original.byte_length);
    // The stored original is untouched by the derivative.
    assert_eq!(std::fs::read(&absolute).unwrap(), wav);
    assert_eq!(library.count("export_derivatives").await, 1);
    assert_eq!(library.count("export_events").await, 2);
}

#[tokio::test]
async fn audio_export_refuses_a_candidate_the_recording_does_not_have() {
    let dir = tempfile::tempdir().unwrap();
    let library = seeded(&dir.path().join("library")).await;
    let destination = out_dir(dir.path(), "audio");

    let unknown_asset = library
        .service()
        .export_media(
            &ExportMediaRequest {
                recording_id: recording(TIMED),
                media_asset_id: Some(MediaAssetId::new()),
                destination: destination.clone(),
                include_sanitized_derivative: false,
            },
            NOW,
        )
        .await
        .unwrap_err();
    assert!(matches!(unknown_asset, ExportError::RecordNotFound));

    let no_media = library
        .service()
        .export_media(
            &ExportMediaRequest {
                recording_id: recording(TEXT_ONLY),
                media_asset_id: None,
                destination: destination.clone(),
                include_sanitized_derivative: false,
            },
            NOW,
        )
        .await
        .unwrap_err();
    assert!(matches!(no_media, ExportError::RecordNotFound));
    assert_eq!(std::fs::read_dir(&destination).unwrap().count(), 0);
}

// ---------------------------------------------------------------------------
// create_portable_backup / inspect / restore
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_plaintext_backup_round_trips_into_a_clean_library() {
    let dir = tempfile::tempdir().unwrap();
    let library = seeded(&dir.path().join("source")).await;
    let backups = out_dir(dir.path(), "backups");

    let response = library
        .service()
        .create_portable_backup(
            &BackupRequest {
                destination: backups,
                password: None,
                sensitive_profile: false,
                include_voice_profiles: false,
            },
            NOW,
        )
        .await
        .unwrap();

    assert!(!response.encrypted);
    assert!(response.protection_disclosure.contains("NOT encrypted"));
    assert_eq!(response.voice_profiles_included, 0);
    assert!(response.entry_count > 0);
    assert_eq!(
        response.manifest_schema_version,
        plaudon_export::archive::MANIFEST_SCHEMA_VERSION
    );
    assert_eq!(library.count("archive_backups").await, 1);

    let report =
        ExportService::inspect_archive_only(&response.path, None, &dir.path().join("probe"))
            .unwrap();
    assert!(report.payloads_verified);
    assert_eq!(report.manifest.integrity_digest, response.integrity_digest);
    assert!(
        !dir.path().join("probe").exists(),
        "inspection wrote nothing"
    );

    // A dry run against the library the archive came from reports every record
    // as a conflict, and still writes nothing.
    let preview = library
        .service()
        .inspect_restore_archive(&InspectArchiveRequest {
            archive_path: response.path.clone(),
            password: None,
        })
        .await
        .unwrap();
    assert!(!preview.target_is_empty);
    assert_eq!(preview.conflicts.len(), 2);

    // Into a clean library it restores completely.
    let restored = Library::empty(&dir.path().join("restored")).await;
    let report = restored
        .service()
        .restore_portable_archive(
            &RestoreArchiveRequest {
                archive_path: response.path.clone(),
                password: None,
                options: RestoreOptions {
                    mode: RestoreMode::NewLibrary,
                    on_conflict: ConflictPolicy::Fail,
                    restore_tombstoned: false,
                },
            },
            NOW,
        )
        .await
        .unwrap();

    assert_eq!(report.recordings_restored, 2);
    assert_eq!(report.transcripts_restored, 2);
    assert_eq!(report.outputs_restored, 1);
    assert_eq!(report.sources_restored, 1);
    assert_eq!(report.media_committed, 1);
    assert_eq!(restored.count("transcript_segments").await, 4);
    assert_eq!(restored.count("transcript_words").await, 1);
    assert_eq!(restored.count("output_claims").await, 1);
    assert_eq!(restored.count("output_evidence").await, 1);
    assert_eq!(restored.count("archive_backups").await, 1);

    let direction: String = sqlx::query("SELECT direction FROM archive_backups")
        .fetch_one(restored.db.pool())
        .await
        .unwrap()
        .get(0);
    assert_eq!(direction, "restore");
}

#[tokio::test]
async fn an_encrypted_backup_needs_its_password_to_be_inspected_or_restored() {
    let dir = tempfile::tempdir().unwrap();
    let library = seeded(&dir.path().join("source")).await;
    let backups = out_dir(dir.path(), "backups");

    let response = library
        .service()
        .create_portable_backup(
            &BackupRequest {
                destination: backups,
                password: Some(PASSWORD.to_owned()),
                sensitive_profile: false,
                include_voice_profiles: false,
            },
            NOW,
        )
        .await
        .unwrap();
    assert!(response.encrypted);
    assert!(response.protection_disclosure.contains("encrypted"));

    let service = library.service();
    assert!(matches!(
        service
            .inspect_restore_archive(&InspectArchiveRequest {
                archive_path: response.path.clone(),
                password: None,
            })
            .await
            .unwrap_err(),
        ExportError::PasswordRequired
    ));
    assert!(matches!(
        service
            .inspect_restore_archive(&InspectArchiveRequest {
                archive_path: response.path.clone(),
                password: Some("wrong".to_owned()),
            })
            .await
            .unwrap_err(),
        ExportError::DecryptionFailed
    ));

    let preview = service
        .inspect_restore_archive(&InspectArchiveRequest {
            archive_path: response.path.clone(),
            password: Some(PASSWORD.to_owned()),
        })
        .await
        .unwrap();
    assert!(preview.inspection.encrypted);
}

#[tokio::test]
async fn voice_profiles_are_refused_without_a_password_and_nothing_is_written() {
    let dir = tempfile::tempdir().unwrap();
    let library = seeded(&dir.path().join("source")).await;
    seed_voice_profile(&library).await;
    let backups = out_dir(dir.path(), "backups");

    let error = library
        .service()
        .create_portable_backup(
            &BackupRequest {
                destination: backups.clone(),
                password: None,
                sensitive_profile: false,
                include_voice_profiles: true,
            },
            NOW,
        )
        .await
        .unwrap_err();

    assert!(matches!(error, ExportError::VoiceProfilesRequireEncryption));
    assert_eq!(std::fs::read_dir(&backups).unwrap().count(), 0);
    assert_eq!(library.count("archive_backups").await, 0);
}

#[tokio::test]
async fn an_encrypted_backup_carries_the_biometric_tier_only_when_it_is_asked_for() {
    let dir = tempfile::tempdir().unwrap();
    let library = seeded(&dir.path().join("source")).await;
    seed_voice_profile(&library).await;

    let without = library
        .service()
        .create_portable_backup(
            &BackupRequest {
                destination: out_dir(dir.path(), "backups-without"),
                password: Some(PASSWORD.to_owned()),
                sensitive_profile: false,
                include_voice_profiles: false,
            },
            NOW,
        )
        .await
        .unwrap();
    assert_eq!(without.voice_profiles_included, 0);

    let with = library
        .service()
        .create_portable_backup(
            &BackupRequest {
                destination: out_dir(dir.path(), "backups-with"),
                password: Some(PASSWORD.to_owned()),
                sensitive_profile: true,
                include_voice_profiles: true,
            },
            NOW,
        )
        .await
        .unwrap();
    assert_eq!(with.voice_profiles_included, 1);
    assert!(with.entry_count > without.entry_count);

    let report =
        ExportService::inspect_archive_only(&with.path, Some(PASSWORD), &dir.path().join("probe"))
            .unwrap();
    let namespaces: Vec<&str> = report
        .entries_by_namespace
        .iter()
        .map(|(name, _)| name.as_str())
        .collect();
    assert!(namespaces.contains(&"voiceprofiles"), "{namespaces:?}");
    assert!(namespaces.contains(&"people"));

    // The roster travels either way; only the biometric tier is conditional.
    let without_report = ExportService::inspect_archive_only(
        &without.path,
        Some(PASSWORD),
        &dir.path().join("probe2"),
    )
    .unwrap();
    let plain: Vec<&str> = without_report
        .entries_by_namespace
        .iter()
        .map(|(name, _)| name.as_str())
        .collect();
    assert!(plain.contains(&"people"));
    assert!(!plain.contains(&"voiceprofiles"), "{plain:?}");
}

#[tokio::test]
async fn restoring_a_damaged_archive_reports_the_damage_and_records_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let library = seeded(&dir.path().join("source")).await;
    let backups = out_dir(dir.path(), "backups");
    let response = library
        .service()
        .create_portable_backup(
            &BackupRequest {
                destination: backups,
                password: None,
                sensitive_profile: false,
                include_voice_profiles: false,
            },
            NOW,
        )
        .await
        .unwrap();

    let mut bytes = std::fs::read(&response.path).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 0xff;
    std::fs::write(&response.path, &bytes).unwrap();

    let restored = Library::empty(&dir.path().join("restored")).await;
    let error = restored
        .service()
        .restore_portable_archive(
            &RestoreArchiveRequest {
                archive_path: response.path.clone(),
                password: None,
                options: RestoreOptions::default(),
            },
            NOW,
        )
        .await
        .unwrap_err();

    assert!(
        matches!(error, ExportError::ArchiveRejected(_)),
        "unexpected {error:?}"
    );
    assert_eq!(restored.count("recordings").await, 0);
    assert_eq!(restored.count("archive_backups").await, 0);
}

#[tokio::test]
async fn restoring_a_file_that_is_not_an_archive_is_refused_before_the_library_is_read() {
    let dir = tempfile::tempdir().unwrap();
    let library = Library::empty(&dir.path().join("library")).await;
    let decoy = dir.path().join("holiday.recording-archive");
    std::fs::write(&decoy, b"PK\x03\x04 not ours").unwrap();

    let error = library
        .service()
        .restore_portable_archive(
            &RestoreArchiveRequest {
                archive_path: decoy,
                password: None,
                options: RestoreOptions::default(),
            },
            NOW,
        )
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        ExportError::ArchiveRejected(ArchiveRejection::NotAnArchive)
    ));
    assert_eq!(library.count("archive_backups").await, 0);
}

// ---------------------------------------------------------------------------
// Destination mechanics
// ---------------------------------------------------------------------------

#[test]
fn a_generated_name_keeps_two_recordings_with_one_title_apart_and_drops_the_title_extension() {
    assert_eq!(
        build_file_name("Notes", "abcd1234", "txt"),
        "Notes-abcd1234.txt"
    );
    assert_eq!(build_file_name("Notes", "", "txt"), "Notes.txt");
    // An empty or whitespace-only title still produces a usable, suffixed name.
    let fallback = build_file_name("", "abcd1234", "txt");
    assert!(fallback.ends_with("-abcd1234.txt"), "{fallback}");
    assert!(!fallback.starts_with('-'), "{fallback}");
    let blank = build_file_name("   ", "", "md");
    assert_eq!(Path::new(&blank).extension().unwrap(), "md");
    assert!(!blank.starts_with('.'), "{blank}");
    assert_eq!(build_file_name("notes.exe", "x", "txt"), "notes-x.txt");
    assert!(!build_file_name("../../etc/passwd", "x", "txt").contains('/'));

    // A very long title is truncated and never ends in a dot or a space.
    let long = build_file_name(&"a".repeat(300), "x", "txt");
    assert!(long.len() < 120, "{long}");
    assert!(long.ends_with("-x.txt"));
}

#[test]
fn a_destination_refuses_to_overwrite_anything_that_is_not_a_plain_file() {
    let dir = tempfile::tempdir().unwrap();
    let destination = ExportDestination::new(dir.path()).unwrap();
    assert_eq!(destination.directory(), dir.path().canonicalize().unwrap());
    assert_ne!(destination.hint(), String::new());

    // A fresh write, then a replacing write: the second one wins and no
    // temporary file is left behind.
    let first = destination.write_atomic("notes.txt", b"one").unwrap();
    let second = destination
        .write_atomic("notes.txt", b"two is longer")
        .unwrap();
    assert_eq!(first, second);
    assert_eq!(std::fs::read(&second).unwrap(), b"two is longer");
    let leftovers = std::fs::read_dir(dir.path())
        .unwrap()
        .filter(|entry| {
            entry
                .as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".part")
        })
        .count();
    assert_eq!(leftovers, 0);

    // A directory sitting at the target name is refused rather than replaced.
    std::fs::create_dir(dir.path().join("folder.txt")).unwrap();
    let error = destination.write_atomic("folder.txt", b"x").unwrap_err();
    assert!(matches!(error, ExportError::DestinationRejected(_)));

    // A failure inside the fill closure removes the temporary file.
    let error = destination
        .write_atomic_with("streamed.txt", |_| Err(ExportError::IntegrityFailed))
        .unwrap_err();
    assert!(matches!(error, ExportError::IntegrityFailed));
    assert!(!dir.path().join("streamed.txt").exists());
    assert_eq!(
        std::fs::read_dir(dir.path())
            .unwrap()
            .filter(|entry| entry
                .as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".part"))
            .count(),
        0
    );
}

// ---------------------------------------------------------------------------
// The projection every export renders from
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_projection_hides_a_deleted_recording_unless_it_is_asked_for() {
    let dir = tempfile::tempdir().unwrap();
    let library = seeded(&dir.path().join("library")).await;
    sqlx::query("UPDATE recordings SET deleted_at = 99 WHERE id = ?1")
        .bind(TEXT_ONLY)
        .execute(library.db.pool())
        .await
        .unwrap();

    let loader = BundleLoader::new(library.db.clone());
    let live = loader.recording_ids(false).await.unwrap();
    assert_eq!(live, vec![recording(TIMED)]);

    let all = loader.recording_ids(true).await.unwrap();
    assert_eq!(all.len(), 2);
    assert!(all.contains(&recording(TEXT_ONLY)));

    // A tombstoned recording still loads when it is named explicitly: deletion
    // is a library state, not a hole in the projection.
    let export = loader.load(recording(TEXT_ONLY), None, NOW).await.unwrap();
    assert_eq!(
        export.recording.deleted_at,
        Some(Timestamp::from_millis(99))
    );
    assert_eq!(export.media.len(), 0);
    assert_eq!(export.sources.len(), 0);
    assert_eq!(export.notices, ExportNotices::default());

    // The timed recording carries the whole projection, and the selected
    // revision is the newest one.
    let full = loader.load(recording(TIMED), None, NOW).await.unwrap();
    assert_eq!(full.media.len(), 1);
    assert_eq!(full.sources.len(), 1);
    assert_eq!(full.speakers.len(), 2);
    assert_eq!(full.transcripts.len(), 1);
    let selected = full.selected_transcript().unwrap();
    assert_eq!(selected.segments.len(), 3);
    assert_eq!(selected.plain_text().lines().count(), 3);
    assert_eq!(full.outputs_for(selected.id).len(), 1);
    assert_eq!(full.outputs_for(TranscriptRevisionId::new()).len(), 0);
    assert!(full.speaker_display("speaker-1").is_some());
    assert!(full.speaker_display("no-such-speaker").is_none());

    // An inferred cluster never leaves with the guessed name attached.
    let inferred = full
        .speakers
        .iter()
        .find(|speaker| speaker.id == "speaker-2")
        .unwrap();
    assert_eq!(inferred.name, None);
    assert!(!inferred.user_confirmed);
}

#[tokio::test]
async fn the_snapshot_reads_tool_definitions_and_refuses_an_unknown_recording() {
    let dir = tempfile::tempdir().unwrap();
    let library = seeded(&dir.path().join("library")).await;
    sqlx::query(
        "INSERT INTO transcript_tools
             (tool_id, version, display_name, kind, prompt_version, output_schema_version,
              definition_sha256, origin, optional, created_at)
         VALUES ('action-items', 1, 'Action items', 'extraction', '1', 'action-items/1',
                 ?1, 'builtin', 0, 5)",
    )
    .bind(format!("tool-definition-{}", "z".repeat(48)))
    .execute(library.db.pool())
    .await
    .unwrap();

    let snapshot = LibrarySnapshot::new(library.db.clone());
    let tools = snapshot.tool_documents().await.unwrap();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].id, "action-items");
    assert_eq!(tools[0].version, 1);
    assert_eq!(tools[0].optional, 0);

    let people = snapshot.people_documents().await.unwrap();
    assert_eq!(people.len(), 0);
    let profiles = snapshot.voice_profile_documents().await.unwrap();
    assert_eq!(profiles.len(), 0);

    // Tombstoned recordings are not archived at all (spec 7.10).
    sqlx::query("UPDATE recordings SET deleted_at = 99 WHERE id = ?1")
        .bind(TEXT_ONLY)
        .execute(library.db.pool())
        .await
        .unwrap();
    assert_eq!(
        snapshot.recording_ids().await.unwrap(),
        vec![TIMED.to_owned()]
    );

    // A recording that does not exist is refused rather than returned empty.
    assert!(matches!(
        snapshot
            .record_document("no-such-recording")
            .await
            .unwrap_err(),
        ExportError::RecordNotFound
    ));
    assert!(matches!(
        snapshot
            .annotation_document("no-such-recording")
            .await
            .unwrap_err(),
        ExportError::RecordNotFound
    ));
    assert!(matches!(
        snapshot
            .media_relative_path("no-such-asset")
            .await
            .unwrap_err(),
        ExportError::RecordNotFound
    ));

    // The backup that follows carries the tool definition.
    let response = library
        .service()
        .create_portable_backup(
            &BackupRequest {
                destination: out_dir(dir.path(), "backups"),
                password: None,
                sensitive_profile: false,
                include_voice_profiles: false,
            },
            NOW,
        )
        .await
        .unwrap();
    let report =
        ExportService::inspect_archive_only(&response.path, None, &dir.path().join("probe"))
            .unwrap();
    assert!(report
        .manifest
        .entries
        .iter()
        .any(|entry| entry.path.starts_with("tools/")));
}

#[test]
fn an_archive_extension_is_reduced_to_a_short_alphanumeric_suffix() {
    assert_eq!(extension_of("media/sha256/aa/aa.wav"), "wav");
    assert_eq!(extension_of("media/sha256/aa/aa.WAV"), "wav");
    assert_eq!(extension_of("media/sha256/aa/aa"), "bin");
    assert_eq!(extension_of("media/sha256/aa/aa."), "bin");
    assert_eq!(extension_of("media/sha256/aa/aa.verylongextension"), "bin");
    assert_eq!(extension_of("media/sha256/aa/aa.w a v"), "bin");
    assert_eq!(extension_of(""), "bin");
}
