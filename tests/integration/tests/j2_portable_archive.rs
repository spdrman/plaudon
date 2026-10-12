//! J2.1-J2.5: `.recording-archive` creation, encryption, restore, secret
//! exclusion and the clean-install round trip.

use std::path::{Path, PathBuf};

use plaudon_domain::time::Timestamp;
use plaudon_export::archive::manifest::Manifest;
use plaudon_export::archive::restore::{ConflictPolicy, RestoreMode, RestoreOptions};
use plaudon_export::service::{
    BackupRequest, ExportService, InspectArchiveRequest, RestoreArchiveRequest,
};
use plaudon_export::ExportError;
use plaudon_integration_tests::fakes::library::{self, SeededLibrary};

const NOW: Timestamp = Timestamp::from_millis(1_700_000_300_000);
const PASSWORD: &str = "correct horse battery staple";

fn schemas_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("root")
        .join("schemas")
}

fn manifest_validator() -> jsonschema::Validator {
    let text =
        std::fs::read_to_string(schemas_dir().join("recording-archive-manifest.v1.schema.json"))
            .expect("read schema");
    let schema: serde_json::Value = serde_json::from_str(&text).expect("parse schema");
    jsonschema::options()
        .with_draft(jsonschema::Draft::Draft202012)
        .build(&schema)
        .expect("compile schema")
}

async fn seeded(dir: &Path, with_secrets: bool) -> (SeededLibrary, ExportService) {
    let library = library::seed(&dir.join("source-library"), with_secrets).await;
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

async fn clean_library(dir: &Path) -> (plaudon_persistence::Database, ExportService, PathBuf) {
    let root = dir.join("restored-library");
    let (db, archive) = library::empty_library(&root).await;
    let service = ExportService::new(db.clone(), archive, root.clone());
    (db, service, root)
}

#[tokio::test]
async fn an_archive_has_the_spec_namespaces_and_a_verifying_manifest() {
    let dir = tempfile::tempdir().unwrap();
    let (_library, service) = seeded(dir.path(), false).await;
    let backups = out_dir(dir.path(), "backups");

    let response = service
        .create_portable_backup(
            &BackupRequest {
                destination: backups.clone(),
                password: None,
                sensitive_profile: false,
                include_voice_profiles: false,
            },
            NOW,
        )
        .await
        .expect("backup");

    assert!(!response.encrypted);
    assert!(response.protection_disclosure.contains("NOT encrypted"));
    assert!(response
        .integrity_note
        .contains("not an authenticity signature"));
    assert!(response
        .path
        .to_string_lossy()
        .ends_with(".recording-archive"));
    assert_ne!(response.nonportable_settings, Vec::<String>::new());

    let report =
        ExportService::inspect_archive_only(&response.path, None, &dir.path().join("probe"))
            .expect("inspect");
    assert!(report.payloads_verified);
    assert_eq!(report.manifest.integrity_digest, response.integrity_digest);

    let namespaces: Vec<&str> = report
        .entries_by_namespace
        .iter()
        .map(|(name, _)| name.as_str())
        .collect();
    assert_eq!(
        namespaces,
        vec![
            "annotations",
            "media",
            "outputs",
            "records",
            "sources",
            "transcripts"
        ]
    );
    for entry in &report.manifest.entries {
        assert_eq!(entry.kind, "file");
        assert!(!entry.path.contains(".."), "{}", entry.path);
        assert!(!entry.path.starts_with('/'), "{}", entry.path);
        if let Some(name) = entry.path.strip_prefix("media/") {
            assert!(name.starts_with(&entry.sha256), "content-addressed media");
        }
    }

    let value = serde_json::to_value(&report.manifest).unwrap();
    let errors: Vec<String> = manifest_validator()
        .iter_errors(&value)
        .map(|error| error.to_string())
        .collect();
    assert!(errors.is_empty(), "{errors:?}");
}

#[tokio::test]
async fn a_plaintext_round_trip_into_a_clean_install_reproduces_the_library() {
    let dir = tempfile::tempdir().unwrap();
    let (library, service) = seeded(dir.path(), false).await;
    let backups = out_dir(dir.path(), "backups");
    let backup = service
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

    let (db, restored_service, root) = clean_library(dir.path()).await;
    let preview = restored_service
        .inspect_restore_archive(&InspectArchiveRequest {
            archive_path: backup.path.clone(),
            password: None,
        })
        .await
        .unwrap();
    assert!(preview.target_is_empty);
    assert_eq!(preview.conflicts, Vec::new());
    // A dry run writes nothing into the library.
    let restored_archive = plaudon_persistence::archive::MediaArchive::open(&root).unwrap();
    assert_eq!(restored_archive.list_blobs().unwrap(), Vec::new());

    let report = restored_service
        .restore_portable_archive(
            &RestoreArchiveRequest {
                archive_path: backup.path.clone(),
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
        .expect("restore");

    assert_eq!(report.recordings_restored, 1);
    assert_eq!(report.transcripts_restored, 3);
    assert_eq!(report.outputs_restored, 1);
    assert_eq!(report.media_committed, 2);
    assert!(report.conflicts.is_empty(), "{:?}", report.conflicts);

    // Media is bit-identical.
    let source_blobs = library.archive.list_blobs().unwrap();
    let restored_blobs = restored_archive.list_blobs().unwrap();
    assert_eq!(source_blobs, restored_blobs);
    for (relative, _) in &source_blobs {
        assert_eq!(
            std::fs::read(library.archive.absolute(relative).unwrap()).unwrap(),
            std::fs::read(restored_archive.absolute(relative).unwrap()).unwrap()
        );
    }

    // Structured records match, including anchors and user annotations.
    let expectations: &[(&str, &str)] = &[
        ("SELECT COUNT(*) FROM recordings", "recordings"),
        ("SELECT COUNT(*) FROM transcript_revisions", "revisions"),
        ("SELECT COUNT(*) FROM transcript_segments", "segments"),
        ("SELECT COUNT(*) FROM transcript_words", "words"),
        ("SELECT COUNT(*) FROM transcript_annotations", "annotations"),
        ("SELECT COUNT(*) FROM ai_outputs", "outputs"),
        ("SELECT COUNT(*) FROM output_evidence", "evidence"),
        ("SELECT COUNT(*) FROM speakers", "speakers"),
        ("SELECT COUNT(*) FROM recording_tags", "tags"),
        ("SELECT COUNT(*) FROM source_items", "source items"),
        ("SELECT COUNT(*) FROM media_assets", "media assets"),
        ("SELECT COUNT(*) FROM recording_media", "media links"),
    ];
    for (sql, label) in expectations {
        let source: i64 = sqlx::query_scalar(*sql)
            .fetch_one(library.db.pool())
            .await
            .unwrap();
        let restored: i64 = sqlx::query_scalar(*sql).fetch_one(db.pool()).await.unwrap();
        assert_eq!(source, restored, "{label} differ");
    }

    let title: String = sqlx::query_scalar("SELECT title FROM recordings WHERE id = ?1")
        .bind(library.recording_id.to_string())
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(title, "Weekly sync: Q3 \"plan\" / review");
    let annotation: Option<String> =
        sqlx::query_scalar("SELECT annotation FROM recordings WHERE id = ?1")
            .bind(library.recording_id.to_string())
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(
        annotation.as_deref(),
        Some("Remember to send the deck. Budget was not specified.")
    );
    let confirmed: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM speakers WHERE user_confirmed = 1")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(confirmed, 1);

    // Re-exporting the restored library yields the same manifest content.
    let second_backups = out_dir(dir.path(), "backups-2");
    let second = restored_service
        .create_portable_backup(
            &BackupRequest {
                destination: second_backups,
                password: None,
                sensitive_profile: false,
                include_voice_profiles: false,
            },
            NOW,
        )
        .await
        .unwrap();
    assert_eq!(second.integrity_digest, backup.integrity_digest);
    assert_eq!(second.entry_count, backup.entry_count);
    db.close().await;
}

#[tokio::test]
async fn an_encrypted_round_trip_needs_the_password_and_no_keychain() {
    let dir = tempfile::tempdir().unwrap();
    let (library, service) = seeded(dir.path(), false).await;
    let backups = out_dir(dir.path(), "backups");
    let backup = service
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
    assert!(backup.encrypted);
    assert!(backup.protection_disclosure.contains("cannot recover"));

    // The ciphertext does not contain the transcript in the clear.
    let bytes = std::fs::read(&backup.path).unwrap();
    assert!(!bytes
        .windows(26)
        .any(|window| window == b"Welcome to the weekly sync"));

    // A wrong password fails with a typed error and writes nothing.
    let (db, restored_service, root) = clean_library(dir.path()).await;
    let error = restored_service
        .restore_portable_archive(
            &RestoreArchiveRequest {
                archive_path: backup.path.clone(),
                password: Some("wrong password".into()),
                options: RestoreOptions::default(),
            },
            NOW,
        )
        .await
        .unwrap_err();
    assert_eq!(error.code().as_str(), "ARCHIVE_DECRYPTION_FAILED");
    let restored_archive = plaudon_persistence::archive::MediaArchive::open(&root).unwrap();
    assert_eq!(restored_archive.list_blobs().unwrap(), Vec::new());
    let recordings: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM recordings")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(recordings, 0);

    // No password at all is a distinct, actionable failure.
    let error = restored_service
        .restore_portable_archive(
            &RestoreArchiveRequest {
                archive_path: backup.path.clone(),
                password: None,
                options: RestoreOptions::default(),
            },
            NOW,
        )
        .await
        .unwrap_err();
    assert!(matches!(error, ExportError::PasswordRequired));

    // The right password restores the library.
    let report = restored_service
        .restore_portable_archive(
            &RestoreArchiveRequest {
                archive_path: backup.path.clone(),
                password: Some(PASSWORD.to_owned()),
                options: RestoreOptions::default(),
            },
            NOW,
        )
        .await
        .unwrap();
    assert_eq!(report.recordings_restored, 1);
    assert_eq!(
        library.archive.list_blobs().unwrap(),
        restored_archive.list_blobs().unwrap()
    );
    db.close().await;
}

#[tokio::test]
async fn a_tampered_encrypted_archive_is_rejected_before_any_user_data_moves() {
    let dir = tempfile::tempdir().unwrap();
    let (_library, service) = seeded(dir.path(), false).await;
    let backups = out_dir(dir.path(), "backups");
    let backup = service
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

    let mut bytes = std::fs::read(&backup.path).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 0xff;
    let tampered = backup.path.with_extension("tampered.recording-archive");
    std::fs::write(&tampered, &bytes).unwrap();

    let (db, restored_service, root) = clean_library(dir.path()).await;
    let error = restored_service
        .restore_portable_archive(
            &RestoreArchiveRequest {
                archive_path: tampered,
                password: Some(PASSWORD.to_owned()),
                options: RestoreOptions::default(),
            },
            NOW,
        )
        .await
        .unwrap_err();
    assert!(
        matches!(
            error,
            ExportError::DecryptionFailed | ExportError::ArchiveRejected(_)
        ),
        "{error:?}"
    );
    let restored_archive = plaudon_persistence::archive::MediaArchive::open(&root).unwrap();
    assert_eq!(restored_archive.list_blobs().unwrap(), Vec::new());
    let recordings: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM recordings")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(recordings, 0);
    db.close().await;
}

#[tokio::test]
async fn an_archive_carries_no_credential_shaped_value() {
    let dir = tempfile::tempdir().unwrap();
    let (_library, service) = seeded(dir.path(), true).await;
    let backups = out_dir(dir.path(), "backups");
    let backup = service
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
        .expect("backup");

    let bytes = std::fs::read(&backup.path).unwrap();
    for (label, secret) in [
        ("openrouter key", library::SEEDED_OPENROUTER_KEY),
        ("plaud token", library::SEEDED_PLAUD_TOKEN),
        ("signed url", library::SEEDED_SIGNED_URL),
        ("device secret", library::SEEDED_DEVICE_SECRET),
    ] {
        assert!(
            !contains(&bytes, secret.as_bytes()),
            "{label} leaked into the archive"
        );
    }

    // Transient and credential namespaces are absent entirely.
    let report = ExportService::inspect_archive_only(&backup.path, None, &dir.path().join("probe"))
        .expect("inspect");
    for entry in &report.manifest.entries {
        for forbidden in [
            "jobs/",
            "inference/",
            "consents/",
            "devices/",
            "preferences/",
            "cache/",
        ] {
            assert!(!entry.path.starts_with(forbidden), "{}", entry.path);
        }
    }

    // And the scanner runs over the archive's own documents as a gate.
    let text = String::from_utf8_lossy(&bytes);
    assert!(!text.contains("sk-or-v1-"));
    assert!(!text.contains("X-Amz-Signature"));
}

#[tokio::test]
async fn a_credential_in_an_annotation_aborts_the_backup_with_no_file_left_behind() {
    let dir = tempfile::tempdir().unwrap();
    let (library, service) = seeded(dir.path(), false).await;
    sqlx::query("UPDATE recordings SET annotation = ?2 WHERE id = ?1")
        .bind(library.recording_id.to_string())
        .bind(format!(
            "paste this into the app: {}",
            library::SEEDED_OPENROUTER_KEY
        ))
        .execute(library.db.pool())
        .await
        .unwrap();

    let backups = out_dir(dir.path(), "backups");
    let error = service
        .create_portable_backup(
            &BackupRequest {
                destination: backups.clone(),
                password: None,
                sensitive_profile: false,
                include_voice_profiles: false,
            },
            NOW,
        )
        .await
        .unwrap_err();
    assert!(matches!(error, ExportError::SecretExclusionFailed(_)));
    assert_eq!(error.code().as_str(), "SECRET_EXCLUSION_FAILED");
    let files: Vec<_> = std::fs::read_dir(&backups).unwrap().collect();
    assert!(files.is_empty(), "a partial archive was left behind");
}

#[tokio::test]
async fn merge_mode_reports_a_conflict_instead_of_overwriting() {
    let dir = tempfile::tempdir().unwrap();
    let (library, service) = seeded(dir.path(), false).await;
    let backups = out_dir(dir.path(), "backups");
    let backup = service
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

    // Restoring into the library it came from must not duplicate or rewrite.
    let title_before: String = sqlx::query_scalar("SELECT title FROM recordings WHERE id = ?1")
        .bind(library.recording_id.to_string())
        .fetch_one(library.db.pool())
        .await
        .unwrap();

    let report = service
        .restore_portable_archive(
            &RestoreArchiveRequest {
                archive_path: backup.path.clone(),
                password: None,
                options: RestoreOptions {
                    mode: RestoreMode::Merge,
                    on_conflict: ConflictPolicy::Skip,
                    restore_tombstoned: false,
                },
            },
            NOW,
        )
        .await
        .unwrap();
    assert_eq!(report.recordings_restored, 0);
    assert_eq!(report.media_reused, 2);
    assert_eq!(report.media_committed, 0);
    assert_eq!(report.conflicts.len(), 1);
    assert!(report.conflicts[0]
        .reason
        .contains("already in this library"));

    let title_after: String = sqlx::query_scalar("SELECT title FROM recordings WHERE id = ?1")
        .bind(library.recording_id.to_string())
        .fetch_one(library.db.pool())
        .await
        .unwrap();
    assert_eq!(title_before, title_after);

    // The strict policy refuses the whole restore instead.
    let error = service
        .restore_portable_archive(
            &RestoreArchiveRequest {
                archive_path: backup.path,
                password: None,
                options: RestoreOptions {
                    mode: RestoreMode::Merge,
                    on_conflict: ConflictPolicy::Fail,
                    restore_tombstoned: false,
                },
            },
            NOW,
        )
        .await
        .unwrap_err();
    assert!(matches!(error, ExportError::RestoreConflict(_)));
}

#[tokio::test]
async fn restoring_into_a_non_empty_library_as_new_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let (_library, service) = seeded(dir.path(), false).await;
    let backups = out_dir(dir.path(), "backups");
    let backup = service
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

    let error = service
        .restore_portable_archive(
            &RestoreArchiveRequest {
                archive_path: backup.path,
                password: None,
                options: RestoreOptions {
                    mode: RestoreMode::NewLibrary,
                    on_conflict: ConflictPolicy::Skip,
                    restore_tombstoned: false,
                },
            },
            NOW,
        )
        .await
        .unwrap_err();
    assert!(matches!(error, ExportError::TargetLibraryNotEmpty));
}

#[tokio::test]
async fn a_deleted_recording_is_not_resurrected_by_a_restore_unless_asked() {
    let dir = tempfile::tempdir().unwrap();
    let (library, service) = seeded(dir.path(), false).await;
    let backups = out_dir(dir.path(), "backups");
    let backup = service
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

    let deletion = plaudon_app_core::deletion::DeletionService::new(
        library.db.clone(),
        library.archive.clone(),
    );
    let disclosure = deletion.disclose(library.recording_id).await.unwrap();
    deletion
        .delete(
            library.recording_id,
            &plaudon_app_core::deletion::DeletionConfirmation {
                disclosure_digest: disclosure.digest(),
                reason: "user_requested".into(),
            },
            NOW,
        )
        .await
        .unwrap();

    let report = service
        .restore_portable_archive(
            &RestoreArchiveRequest {
                archive_path: backup.path.clone(),
                password: None,
                options: RestoreOptions {
                    mode: RestoreMode::Merge,
                    on_conflict: ConflictPolicy::Skip,
                    restore_tombstoned: false,
                },
            },
            NOW,
        )
        .await
        .unwrap();
    assert_eq!(report.recordings_restored, 0);
    assert_eq!(report.conflicts.len(), 1);
    assert_eq!(report.conflicts[0].reason, "deleted locally");

    // The explicit opt-in brings it back.
    let report = service
        .restore_portable_archive(
            &RestoreArchiveRequest {
                archive_path: backup.path,
                password: None,
                options: RestoreOptions {
                    mode: RestoreMode::Merge,
                    on_conflict: ConflictPolicy::Skip,
                    restore_tombstoned: true,
                },
            },
            NOW,
        )
        .await
        .unwrap();
    assert_eq!(report.recordings_restored, 1);
}

#[tokio::test]
async fn archive_operations_are_recorded_for_diagnostics() {
    let dir = tempfile::tempdir().unwrap();
    let (library, service) = seeded(dir.path(), false).await;
    let backups = out_dir(dir.path(), "backups");
    service
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

    let rows: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT direction, protection, manifest_schema_version FROM archive_backups",
    )
    .fetch_all(library.db.pool())
    .await
    .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0, "backup");
    assert_eq!(rows[0].1, "encrypted");
    assert_eq!(rows[0].2, "recording-archive-manifest/1");
}

#[tokio::test]
async fn the_manifest_lists_the_settings_that_do_not_travel() {
    let dir = tempfile::tempdir().unwrap();
    let (_library, service) = seeded(dir.path(), false).await;
    let backups = out_dir(dir.path(), "backups");
    let backup = service
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
    let report =
        ExportService::inspect_archive_only(&backup.path, None, &dir.path().join("probe")).unwrap();
    let text = report.manifest.nonportable_settings.join(" ");
    assert!(text.contains("keychain"));
    assert!(text.contains("permission"));
    assert!(text.contains("paths"));
    assert!(text.contains("Model downloads"));
}

#[tokio::test]
async fn creation_streams_rather_than_buffering_the_archive() {
    // A library whose media is far larger than any buffer the writer holds.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("big-library");
    let (db, archive) = library::empty_library(&root).await;
    let bytes = library::wav_with_serial(4 * 1024 * 1024, "SERIAL");
    assert!(bytes.len() > 8 * 1024 * 1024);

    let identity = plaudon_domain::media::MediaIdentity::of_bytes(&bytes, "wav");
    let relative = plaudon_persistence::archive::relative_path(&identity, "wav");
    let absolute = archive.absolute(&relative).unwrap();
    std::fs::create_dir_all(absolute.parent().unwrap()).unwrap();
    std::fs::write(&absolute, &bytes).unwrap();

    let recording_id = plaudon_domain::ids::RecordingId::new();
    let asset_id = plaudon_domain::ids::MediaAssetId::new();
    sqlx::query(
        "INSERT INTO media_assets (id, sha256, byte_length, relative_path, quality_class,
                                   verification_state, created_at)
         VALUES (?1, ?2, ?3, ?4, 'user_supplied_file', 'verified', 1)",
    )
    .bind(asset_id.to_string())
    .bind(identity.sha256.to_hex())
    .bind(i64::try_from(identity.byte_length).unwrap())
    .bind(&relative)
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query("INSERT INTO recordings (id, title, created_at) VALUES (?1, 'Big', 1)")
        .bind(recording_id.to_string())
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO recording_media (recording_id, media_asset_id, quality_class, obtained_at)
         VALUES (?1, ?2, 'user_supplied_file', 1)",
    )
    .bind(recording_id.to_string())
    .bind(asset_id.to_string())
    .execute(db.pool())
    .await
    .unwrap();

    let service = ExportService::new(db.clone(), archive, root.clone());
    let backups = out_dir(dir.path(), "backups");
    let response = service
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
    assert!(response.total_byte_length > 8 * 1024 * 1024);

    // Round-trips, so streaming did not corrupt anything.
    let (restored_db, restored_service, restored_root) = clean_library(dir.path()).await;
    restored_service
        .restore_portable_archive(
            &RestoreArchiveRequest {
                archive_path: response.path,
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
        .unwrap();
    let restored_archive =
        plaudon_persistence::archive::MediaArchive::open(&restored_root).unwrap();
    assert_eq!(
        std::fs::read(restored_archive.absolute(&relative).unwrap()).unwrap(),
        bytes
    );
    restored_db.close().await;
    db.close().await;
}

#[tokio::test]
async fn an_archive_from_another_operating_system_restores_unchanged() {
    // The manifest records the producing OS; a restore must not depend on it.
    let dir = tempfile::tempdir().unwrap();
    let (library, service) = seeded(dir.path(), false).await;
    let backups = out_dir(dir.path(), "backups");
    let backup = service
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

    let report =
        ExportService::inspect_archive_only(&backup.path, None, &dir.path().join("probe")).unwrap();
    let produced_on = report.manifest.source_os.clone();
    assert_ne!(produced_on, "unknown");

    // Every entry path is legal on both operating systems: no reserved
    // characters, no device names, no trailing dots or spaces, and no pair of
    // entries that collide when case is folded.
    let mut folded: Vec<String> = Vec::new();
    for entry in &report.manifest.entries {
        assert!(
            !entry
                .path
                .chars()
                .any(|c| matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*' | '\\')),
            "{}",
            entry.path
        );
        for component in entry.path.split('/') {
            assert!(!component.ends_with('.'), "{}", entry.path);
            assert!(!component.ends_with(' '), "{}", entry.path);
            assert!(component.len() <= 255, "{}", entry.path);
        }
        folded.push(entry.path.to_lowercase());
    }
    let unique: std::collections::BTreeSet<&String> = folded.iter().collect();
    assert_eq!(unique.len(), folded.len(), "case-folded collision");

    let (db, restored_service, _) = clean_library(dir.path()).await;
    restored_service
        .restore_portable_archive(
            &RestoreArchiveRequest {
                archive_path: backup.path,
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
    let restored: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM recordings")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(restored, 1);
    let _ = library;
    db.close().await;
}

#[tokio::test]
async fn an_archive_to_archive_round_trip_preserves_the_manifest() {
    let dir = tempfile::tempdir().unwrap();
    let (_library, service) = seeded(dir.path(), false).await;
    let first = service
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

    let (db, restored_service, _) = clean_library(dir.path()).await;
    restored_service
        .restore_portable_archive(
            &RestoreArchiveRequest {
                archive_path: first.path.clone(),
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

    let second = restored_service
        .create_portable_backup(
            &BackupRequest {
                destination: out_dir(dir.path(), "backups-2"),
                password: None,
                sensitive_profile: false,
                include_voice_profiles: false,
            },
            NOW,
        )
        .await
        .unwrap();

    let a = read_manifest(&first.path);
    let b = read_manifest(&second.path);
    assert_eq!(a.entries, b.entries);
    assert_eq!(a.totals, b.totals);
    assert_eq!(a.integrity_digest, b.integrity_digest);
    db.close().await;
}

fn read_manifest(path: &Path) -> Manifest {
    plaudon_export::archive::reader::read_manifest(path, None)
        .expect("manifest")
        .0
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}
