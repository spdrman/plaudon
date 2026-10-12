//! P2.3: the voice tier in backups, exports and restores
//! (`VOICE_PROFILES_SPEC` VP-077, VP-078; threat-model rows TM-046/TM-048).
//!
//! Every test runs against a real migrated library, a real archive file on
//! disk and the real restorer. Nothing is stubbed: "unrepresentable" is
//! asserted by trying to write the archive and looking at what landed.

use std::path::{Path, PathBuf};

use plaudon_domain::time::Timestamp;
use plaudon_export::archive::restore::{ConflictPolicy, RestoreMode, RestoreOptions};
use plaudon_export::archive::secrets::ArchiveSecretScanner;
use plaudon_export::archive::voiceprofiles::{VoiceProfileDocument, VoiceProfilePermit};
use plaudon_export::archive::writer::{ArchiveBuilder, Protection};
use plaudon_export::service::{
    BackupRequest, ExportFormat, ExportRecordingRequest, ExportService, RestoreArchiveRequest,
};
use plaudon_export::ExportError;
use plaudon_integration_tests::fakes::library::{self, SeededLibrary};
use plaudon_persistence::voice_profiles::VoiceProfileGateRepository;
use plaudon_persistence::Database;
use sqlx::Row;

const NOW: Timestamp = Timestamp::from_millis(1_700_000_300_000);
const PASSWORD: &str = "correct horse battery staple";
const PERSON: &str = "018f3b3a-7b4f-7c21-9a3e-2b1f0c5d6e7a";

/// A sealed 192-dimensional embedding is a kilobyte of ciphertext; this stands
/// in for one, byte for byte the same shape.
fn sealed_bytes() -> Vec<u8> {
    (0..784u32).map(|index| (index % 251) as u8).collect()
}

async fn seeded(dir: &Path) -> (SeededLibrary, ExportService) {
    let library = library::seed(&dir.join("source-library"), false).await;
    let service = ExportService::new(
        library.db.clone(),
        library.archive.clone(),
        library.root.clone(),
    );
    (library, service)
}

fn out_dir(dir: &Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::create_dir_all(&path).unwrap();
    path
}

async fn clean_library(dir: &Path) -> (Database, ExportService) {
    let root = dir.join("restored-library");
    let (db, archive) = library::empty_library(&root).await;
    let service = ExportService::new(db.clone(), archive, root.clone());
    (db, service)
}

/// Enrol one person with one sealed exemplar, cut from the seeded recording.
async fn enrol(library: &SeededLibrary) {
    sqlx::query(
        "INSERT INTO people (id, display_name, aliases_json, email, organisation, created_at,
                             updated_at, status)
         VALUES (?1, 'Priya Raman', '[\"Priya R\"]', 'priya@example.org', 'Acme', 1, 1, 'active')",
    )
    .bind(PERSON)
    .execute(library.db.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO person_voice_enrollments
             (person_id, consent_version, consent_note, retention_months, last_matched_at, status,
              created_at)
         VALUES (?1, 'voice-consent/1', NULL, 12, NULL, 'active', 1)",
    )
    .bind(PERSON)
    .execute(library.db.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO voice_exemplars
             (id, person_id, recording_id, diarization_run_id, cluster_key, source_start_ms,
              source_end_ms, speech_ms, model_id, model_version, model_sha256, embedding_dim,
              embedding_cipher, embedding_nonce, created_at)
         VALUES ('018f3b3a-7b4f-7c21-9a3e-2b1f0c5d6e7b', ?1, ?2, NULL, 'c0', 0, 12000, 12000,
                 'wespeaker-resnet34-lm', '1.0', ?3, 192, ?4, ?5, 1)",
    )
    .bind(PERSON)
    .bind(library.recording_id.to_string())
    .bind("b".repeat(64))
    .bind(sealed_bytes())
    .bind(vec![7u8; 24])
    .execute(library.db.pool())
    .await
    .unwrap();
    // The roster link on the recording's confirmed speaker.
    sqlx::query("UPDATE speakers SET person_id = ?1 WHERE recording_id = ?2 AND name IS NOT NULL")
        .bind(PERSON)
        .bind(library.recording_id.to_string())
        .execute(library.db.pool())
        .await
        .unwrap();
}

async fn count(db: &Database, sql: &str) -> i64 {
    sqlx::query(sqlx::AssertSqlSafe(sql.to_owned()))
        .fetch_one(db.pool())
        .await
        .unwrap()
        .get::<i64, _>(0)
}

#[tokio::test]
async fn a_plaintext_backup_carrying_voice_profiles_cannot_be_produced() {
    let dir = tempfile::tempdir().unwrap();
    let (library, service) = seeded(dir.path()).await;
    enrol(&library).await;
    let backups = out_dir(dir.path(), "backups");

    let refusal = service
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
        .expect_err("a plaintext backup with voice data must be refused");
    assert!(matches!(
        refusal,
        ExportError::VoiceProfilesRequireEncryption
    ));

    // The refusal is before any file is committed: the folder is untouched.
    let written: Vec<_> = std::fs::read_dir(&backups).unwrap().collect();
    assert!(written.is_empty(), "nothing was written");
}

#[tokio::test]
async fn a_staged_voice_entry_cannot_be_emitted_as_a_plaintext_archive() {
    // The second half of the type-level guard: a permit proves the caller asked
    // for encryption, and the builder proves the archive is encrypted. Even a
    // caller that obtained a permit and then changed its mind is refused.
    let dir = tempfile::tempdir().unwrap();
    let staging = out_dir(dir.path(), "staging");
    let destination =
        plaudon_export::destination::ExportDestination::new(&out_dir(dir.path(), "out")).unwrap();
    let encrypted = Protection::Encrypted {
        password: PASSWORD.to_owned(),
        profile: plaudon_export::archive::crypto::KdfProfile::Backup,
    };
    let permit = VoiceProfilePermit::for_protection(&encrypted).unwrap();

    let mut builder = ArchiveBuilder::new(&staging, NOW).unwrap();
    builder
        .add_voice_document(
            &VoiceProfileDocument::entry_path(PERSON),
            br#"{"schema_version":"archive-voiceprofile/1","person_id":"p","enrollment":{},"exemplars":[]}"#,
            permit,
        )
        .unwrap();
    let refusal = builder
        .finish(&destination, "x.recording-archive", &Protection::Plaintext)
        .expect_err("plaintext emission of a voice entry must be refused");
    assert!(matches!(
        refusal,
        ExportError::VoiceProfilesRequireEncryption
    ));
}

#[tokio::test]
async fn an_ordinary_backup_carries_the_roster_and_no_biometric_entry() {
    let dir = tempfile::tempdir().unwrap();
    let (library, service) = seeded(dir.path()).await;
    enrol(&library).await;
    let backups = out_dir(dir.path(), "backups");

    let response = service
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
        .expect("plaintext backup");
    assert_eq!(response.voice_profiles_included, 0);

    let report =
        ExportService::inspect_archive_only(&response.path, None, &dir.path().join("probe"))
            .expect("inspect");
    let paths: Vec<&str> = report
        .manifest
        .entries
        .iter()
        .map(|entry| entry.path.as_str())
        .collect();
    assert!(
        paths.iter().any(|path| path.starts_with("people/")),
        "the roster is ordinary library data (VP-077): {paths:?}"
    );
    assert!(
        !paths.iter().any(|path| path.starts_with("voiceprofiles/")),
        "biometric data is excluded by default: {paths:?}"
    );
}

#[tokio::test]
async fn an_encrypted_backup_may_carry_voice_profiles_and_a_restore_re_runs_the_consent_gate() {
    let dir = tempfile::tempdir().unwrap();
    let (library, service) = seeded(dir.path()).await;
    enrol(&library).await;
    let backups = out_dir(dir.path(), "backups");

    let response = service
        .create_portable_backup(
            &BackupRequest {
                destination: backups,
                password: Some(PASSWORD.to_owned()),
                sensitive_profile: false,
                include_voice_profiles: true,
            },
            NOW,
        )
        .await
        .expect("encrypted backup");
    assert!(response.encrypted);
    assert_eq!(response.voice_profiles_included, 1);

    // The ciphertext is not readable in the file: the body is encrypted.
    let bytes = std::fs::read(&response.path).unwrap();
    let needle = sealed_bytes();
    assert!(
        !bytes.windows(needle.len()).any(|window| window == needle),
        "a sealed embedding must not appear verbatim in the archive file"
    );

    let (db, restored) = clean_library(dir.path()).await;
    let gate = VoiceProfileGateRepository::new(db.clone());
    // The target library had the feature switched on before the restore.
    gate.activate("voice-consent/1", NOW).await.unwrap();

    let report = restored
        .restore_portable_archive(
            &RestoreArchiveRequest {
                archive_path: response.path.clone(),
                password: Some(PASSWORD.to_owned()),
                options: RestoreOptions {
                    mode: RestoreMode::NewLibrary,
                    on_conflict: ConflictPolicy::Fail,
                    restore_tombstoned: false,
                },
            },
            NOW,
        )
        .await
        .expect("restore");

    assert_eq!(report.people_restored, 1);
    assert_eq!(report.voice_profiles_restored, 1);
    assert!(report.voice_consent_required);
    assert_eq!(count(&db, "SELECT COUNT(*) FROM voice_exemplars").await, 1);
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM voice_exemplars WHERE diarization_run_id IS NULL"
        )
        .await,
        1,
        "diarization runs do not travel, so their provenance is dropped"
    );
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM speakers WHERE person_id IS NOT NULL"
        )
        .await,
        1,
        "the roster link travels with the roster"
    );

    // VP-078: the gate is re-armed, matching is off, and nothing about the
    // restored profiles is usable until the user passes it here.
    let after = gate.load().await.unwrap();
    assert!(after.restore_pending_consent);
    assert!(!after.enabled);
    assert!(!after.is_active());
}

#[tokio::test]
async fn the_scanner_refuses_an_embedding_outside_the_voice_namespace() {
    let document = format!(
        r#"{{"schema_version":"archive-record/1","embedding_cipher":"{}"}}"#,
        "A".repeat(1200)
    );
    let refusal = ArchiveSecretScanner::enforce("records/r1.json", document.as_bytes())
        .expect_err("an embedding outside voiceprofiles/ aborts the backup");
    assert!(matches!(refusal, ExportError::SecretExclusionFailed(_)));

    // A blob with an innocent field name is still refused: the shape is enough.
    let blob = format!(r#"{{"annotation":"{}"}}"#, "QUJD".repeat(200));
    assert!(ArchiveSecretScanner::enforce("annotations/r1.json", blob.as_bytes()).is_err());

    // Inside the namespace it is the payload the namespace exists for.
    ArchiveSecretScanner::enforce(
        &VoiceProfileDocument::entry_path(PERSON),
        document.as_bytes(),
    )
    .expect("the encrypted-only namespace carries sealed embeddings");

    // Ordinary documents keep passing: prose, digests and identities.
    let ordinary = format!(
        r#"{{"schema_version":"archive-annotations/1","title":"Weekly sync",
            "annotation":"{}","sha256":"{}","id":"{PERSON}"}}"#,
        "the quick brown fox jumps over the lazy dog ".repeat(60),
        "a".repeat(64)
    );
    ArchiveSecretScanner::enforce("annotations/r1.json", ordinary.as_bytes()).expect("no finding");
}

#[tokio::test]
async fn a_recording_export_carries_names_and_never_voice_data() {
    let dir = tempfile::tempdir().unwrap();
    let (library, service) = seeded(dir.path()).await;
    enrol(&library).await;
    let destination = out_dir(dir.path(), "exports");

    let response = service
        .export_recording(
            &ExportRecordingRequest {
                recording_id: library.recording_id,
                transcript_revision_id: None,
                destination,
                formats: vec![
                    ExportFormat::Txt,
                    ExportFormat::Markdown,
                    ExportFormat::Json,
                ],
            },
            NOW,
        )
        .await
        .expect("export");

    let needle = sealed_bytes();
    for file in &response.files {
        let bytes = std::fs::read(&file.path).unwrap();
        assert!(
            !bytes.windows(needle.len()).any(|window| window == needle),
            "{:?} must not contain embedding bytes",
            file.path
        );
        let text = String::from_utf8_lossy(&bytes).to_ascii_lowercase();
        for forbidden in [
            "embedding",
            "voiceprint",
            "person_id",
            "suggestion",
            "priya@example.org",
        ] {
            assert!(
                !text.contains(forbidden),
                "{:?} must not contain {forbidden} (VP-077: a recording export carries names)",
                file.path
            );
        }
    }
}
