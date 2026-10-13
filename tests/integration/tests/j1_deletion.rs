//! J1.4-J1.5: deletion disclosure, durable tombstones and no resurrection.

use plaudon_app_core::deletion::{DeletionConfirmation, DeletionService};
use plaudon_domain::media::MediaIdentity;
use plaudon_domain::time::Timestamp;
use plaudon_integration_tests::fakes::library::{self, SeededLibrary};
use plaudon_persistence::deletion::{DeletionRecord, DeletionRepository, DeletionState};
use plaudon_persistence::migrations::MigrationRunner;
use plaudon_persistence::Database;
use sqlx::Row;

const NOW: Timestamp = Timestamp::from_millis(1_700_000_200_000);

async fn confirm(service: &DeletionService, library: &SeededLibrary) -> DeletionConfirmation {
    let disclosure = service.disclose(library.recording_id).await.unwrap();
    DeletionConfirmation {
        disclosure_digest: disclosure.digest(),
        reason: "user_requested".into(),
    }
}

async fn dangling_references(db: &Database) -> Vec<String> {
    sqlx::query("PRAGMA foreign_key_check")
        .fetch_all(db.pool())
        .await
        .unwrap()
        .iter()
        .map(|row| row.get::<String, _>(0))
        .collect()
}

#[tokio::test]
async fn the_disclosure_lists_every_category_and_states_what_it_cannot_reach() {
    let dir = tempfile::tempdir().unwrap();
    let library = library::seed(&dir.path().join("library"), false).await;
    let service = DeletionService::new(library.db.clone(), library.archive.clone());

    let disclosure = service.disclose(library.recording_id).await.unwrap();
    assert_eq!(disclosure.media.len(), 2);
    assert!(disclosure.media.iter().all(|m| m.bytes_will_be_removed));
    assert_eq!(disclosure.counts.transcript_revisions, 3);
    assert!(disclosure.counts.transcript_segments >= 7);
    assert!(disclosure.counts.transcript_words > 0);
    assert_eq!(disclosure.counts.ai_outputs, 1);
    assert_eq!(disclosure.counts.output_evidence, 3);
    assert_eq!(disclosure.counts.speakers, 2);
    assert_eq!(disclosure.counts.tags, 2);
    assert_eq!(disclosure.counts.source_items, 1);
    assert!(disclosure.has_annotation);
    assert!(disclosure.bytes_to_free > 0);

    let limits = disclosure.limits.join(" ");
    assert!(limits.contains("Exports you already saved outside this application are not touched"));
    assert!(limits.contains("outside our control"));
    assert!(limits.contains("cannot guarantee"));
    assert!(limits.contains("tombstone"));
}

#[tokio::test]
async fn deletion_removes_managed_derivatives_and_leaves_no_dangling_reference() {
    let dir = tempfile::tempdir().unwrap();
    let library = library::seed(&dir.path().join("library"), false).await;
    let service = DeletionService::new(library.db.clone(), library.archive.clone());
    let blobs_before = library.archive.list_blobs().unwrap();
    assert_eq!(blobs_before.len(), 2);

    let confirmation = confirm(&service, &library).await;
    let receipt = service
        .delete(library.recording_id, &confirmation, NOW)
        .await
        .unwrap();

    assert_eq!(receipt.blobs_removed, 2);
    assert!(receipt.tombstoned);
    assert_eq!(library.archive.list_blobs().unwrap(), Vec::new());
    assert_eq!(dangling_references(&library.db).await, Vec::<String>::new());

    for (table, column) in [
        ("recordings", "id"),
        ("transcript_revisions", "recording_id"),
        ("ai_outputs", "recording_id"),
        ("speakers", "recording_id"),
        ("recording_tags", "recording_id"),
        ("source_items", "recording_id"),
        ("recording_media", "recording_id"),
    ] {
        let count: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT COUNT(*) FROM {table} WHERE {column} = ?1"
        )))
        .bind(library.recording_id.to_string())
        .fetch_one(library.db.pool())
        .await
        .unwrap();
        assert_eq!(count, 0, "{table} still has rows");
    }
    let segments: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM transcript_segments")
        .fetch_one(library.db.pool())
        .await
        .unwrap();
    assert_eq!(segments, 0);
    let words: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM transcript_words")
        .fetch_one(library.db.pool())
        .await
        .unwrap();
    assert_eq!(words, 0);
}

#[tokio::test]
async fn a_stale_disclosure_cannot_authorize_a_deletion() {
    let dir = tempfile::tempdir().unwrap();
    let library = library::seed(&dir.path().join("library"), false).await;
    let service = DeletionService::new(library.db.clone(), library.archive.clone());
    let confirmation = confirm(&service, &library).await;

    // The library changes after the user saw the disclosure.
    sqlx::query("UPDATE recordings SET title = 'Renamed' WHERE id = ?1")
        .bind(library.recording_id.to_string())
        .execute(library.db.pool())
        .await
        .unwrap();

    let error = service
        .delete(library.recording_id, &confirmation, NOW)
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        plaudon_app_core::deletion::DeletionError::ConfirmationMismatch
    ));
    assert_eq!(library.archive.list_blobs().unwrap().len(), 2);
}

#[tokio::test]
async fn bytes_shared_with_another_recording_are_kept() {
    let dir = tempfile::tempdir().unwrap();
    let library = library::seed(&dir.path().join("library"), false).await;

    // A second logical meeting that references the same original blob.
    let other = plaudon_domain::ids::RecordingId::new();
    sqlx::query(
        "INSERT INTO recordings (id, title, created_at) VALUES (?1, 'Other meeting', 1700000000000)",
    )
    .bind(other.to_string())
    .execute(library.db.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO recording_media (recording_id, media_asset_id, quality_class, obtained_at)
         VALUES (?1, ?2, 'original_device_recording', 1700000000000)",
    )
    .bind(other.to_string())
    .bind(library.original_asset.to_string())
    .execute(library.db.pool())
    .await
    .unwrap();

    let service = DeletionService::new(library.db.clone(), library.archive.clone());
    let disclosure = service.disclose(library.recording_id).await.unwrap();
    let shared = disclosure
        .media
        .iter()
        .find(|media| media.sha256 == library.original_identity.sha256.to_hex())
        .unwrap();
    assert!(shared.shared_with_other_recordings);
    assert!(!shared.bytes_will_be_removed);

    let confirmation = DeletionConfirmation {
        disclosure_digest: disclosure.digest(),
        reason: "user_requested".into(),
    };
    let receipt = service
        .delete(library.recording_id, &confirmation, NOW)
        .await
        .unwrap();
    assert_eq!(receipt.blobs_removed, 1);
    assert_eq!(receipt.blobs_retained_shared, 1);

    let remaining = library.archive.list_blobs().unwrap();
    assert_eq!(remaining.len(), 1);
    assert_eq!(
        library::digest_of(&library.archive.absolute(&remaining[0].0).unwrap()),
        library.original_identity.sha256
    );
    assert_eq!(dangling_references(&library.db).await, Vec::<String>::new());
}

#[tokio::test]
async fn a_deleted_recording_is_not_resurrected_by_a_later_sync() {
    let dir = tempfile::tempdir().unwrap();
    let library = library::seed(&dir.path().join("library"), false).await;
    let service = DeletionService::new(library.db.clone(), library.archive.clone());

    let source_id: String =
        sqlx::query_scalar("SELECT source_id FROM source_items WHERE recording_id = ?1")
            .bind(library.recording_id.to_string())
            .fetch_one(library.db.pool())
            .await
            .unwrap();

    let confirmation = confirm(&service, &library).await;
    let receipt = service
        .delete(library.recording_id, &confirmation, NOW)
        .await
        .unwrap();

    // The next scan offers the same bytes and the same external id again.
    assert!(service
        .is_media_suppressed(&library.original_identity)
        .await
        .unwrap());
    assert!(service
        .is_source_item_suppressed(&source_id, "external-7")
        .await
        .unwrap());
    // Different bytes are not suppressed: suppression is identity, not a flag.
    let unrelated = MediaIdentity::of_bytes(b"different recording", "wav");
    assert!(!service.is_media_suppressed(&unrelated).await.unwrap());

    // Only the explicit reimport action clears it.
    // Two media identities plus one source identity.
    assert_eq!(
        service.allow_reimport(&receipt.deletion_id).await.unwrap(),
        3
    );
    assert!(!service
        .is_media_suppressed(&library.original_identity)
        .await
        .unwrap());
    assert!(!service
        .is_source_item_suppressed(&source_id, "external-7")
        .await
        .unwrap());
}

#[tokio::test]
async fn a_remote_trash_event_never_deletes_local_content() {
    let dir = tempfile::tempdir().unwrap();
    let library = library::seed(&dir.path().join("library"), false).await;

    sqlx::query(
        "UPDATE source_items SET remote_state = 'trashed', tombstone_reason = 'remote_trash'
         WHERE recording_id = ?1",
    )
    .bind(library.recording_id.to_string())
    .execute(library.db.pool())
    .await
    .unwrap();

    // Nothing in the deletion path runs for a remote event: the archive and
    // the recording are untouched, and no tombstone exists.
    assert_eq!(library.archive.list_blobs().unwrap().len(), 2);
    let tombstones: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tombstones")
        .fetch_one(library.db.pool())
        .await
        .unwrap();
    assert_eq!(tombstones, 0);
    let service = DeletionService::new(library.db.clone(), library.archive.clone());
    assert!(!service
        .is_media_suppressed(&library.original_identity)
        .await
        .unwrap());
}

#[tokio::test]
async fn an_interrupted_deletion_is_finished_by_the_reconciler() {
    let dir = tempfile::tempdir().unwrap();
    let library = library::seed(&dir.path().join("library"), false).await;
    let service = DeletionService::new(library.db.clone(), library.archive.clone());
    let disclosure = service.disclose(library.recording_id).await.unwrap();

    // Simulate a crash immediately after the journal row committed: the
    // intent is durable, nothing else has happened.
    let journal = DeletionRepository::new(library.db.clone());
    journal
        .open(
            &DeletionRecord {
                id: "interrupted".into(),
                recording_id: library.recording_id,
                reason: "user_requested".into(),
                state: DeletionState::Planned,
                disclosure_json: serde_json::to_string(&disclosure).unwrap(),
                created_at: NOW,
                completed_at: None,
            },
            &disclosure
                .media
                .iter()
                .map(|media| plaudon_persistence::deletion::PendingBlob {
                    relative_path: media.relative_path.clone(),
                    identity: MediaIdentity::new(
                        media.sha256.parse().unwrap(),
                        media.byte_length,
                        String::new(),
                    ),
                    removed_at: None,
                })
                .collect::<Vec<_>>(),
        )
        .await
        .unwrap();

    assert_eq!(journal.unfinished().await.unwrap().len(), 1);
    assert_eq!(library.archive.list_blobs().unwrap().len(), 2);

    let receipts = service.reconcile(NOW).await.unwrap();
    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0].blobs_removed, 2);
    assert_eq!(journal.unfinished().await.unwrap(), Vec::new());
    assert_eq!(library.archive.list_blobs().unwrap(), Vec::new());
    assert_eq!(dangling_references(&library.db).await, Vec::<String>::new());

    // Reconciling again is a no-op rather than an error.
    assert_eq!(service.reconcile(NOW).await.unwrap(), Vec::new());
}

#[tokio::test]
async fn tombstones_and_suppressions_survive_restart_and_migration() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("library");
    let library = library::seed(&root, false).await;
    let service = DeletionService::new(library.db.clone(), library.archive.clone());
    let confirmation = confirm(&service, &library).await;
    service
        .delete(library.recording_id, &confirmation, NOW)
        .await
        .unwrap();
    let identity = library.original_identity.clone();
    let recording_id = library.recording_id;
    library.db.close().await;

    // Reopen and re-run migrations, as a version upgrade would.
    let db = Database::open(root.join("app.sqlite")).await.unwrap();
    MigrationRunner::new().run(&db).await.unwrap();
    let reopened = DeletionService::new(
        db.clone(),
        plaudon_persistence::archive::MediaArchive::open(&root).unwrap(),
    );
    assert!(reopened.is_media_suppressed(&identity).await.unwrap());

    let tombstone: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM tombstones WHERE entity_type = 'recording' AND entity_id = ?1",
    )
    .bind(recording_id.to_string())
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(tombstone, 1);

    let completed: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM deletion_records WHERE state = 'completed' AND recording_id = ?1",
    )
    .bind(recording_id.to_string())
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(completed, 1);
    db.close().await;
}

#[tokio::test]
async fn deleting_twice_is_refused_rather_than_destructive() {
    let dir = tempfile::tempdir().unwrap();
    let library = library::seed(&dir.path().join("library"), false).await;
    let service = DeletionService::new(library.db.clone(), library.archive.clone());
    let confirmation = confirm(&service, &library).await;
    service
        .delete(library.recording_id, &confirmation, NOW)
        .await
        .unwrap();

    let error = service
        .delete(library.recording_id, &confirmation, NOW)
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        plaudon_app_core::deletion::DeletionError::RecordNotFound
    ));
}

#[tokio::test]
async fn deleting_a_recording_cascades_its_suggestions_and_keeps_the_enrolment() {
    // VP-080: a suggestion is a statement about this recording and dies with
    // it; an exemplar belongs to the person the user enrolled and survives with
    // its provenance forgotten. The disclosure says both, with exact counts.
    let dir = tempfile::tempdir().unwrap();
    let library = library::seed(&dir.path().join("library"), false).await;
    let person = "018f3b3a-7b4f-7c21-9a3e-2b1f0c5d6e7a";
    sqlx::query(
        "INSERT INTO people (id, display_name, created_at, updated_at, status)
         VALUES (?1, 'Priya', 1, 1, 'active')",
    )
    .bind(person)
    .execute(library.db.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO person_voice_enrollments
             (person_id, consent_version, retention_months, status, created_at)
         VALUES (?1, 'voice-consent/1', 12, 'active', 1)",
    )
    .bind(person)
    .execute(library.db.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO voice_exemplars
             (id, person_id, recording_id, cluster_key, source_start_ms, source_end_ms, speech_ms,
              model_id, model_version, model_sha256, embedding_dim, embedding_cipher,
              embedding_nonce, created_at)
         VALUES ('x1', ?1, ?2, 'c0', 0, 9000, 9000, 'wespeaker-resnet34-lm', '1.0', ?3, 192, ?4,
                 ?5, 1)",
    )
    .bind(person)
    .bind(library.recording_id.to_string())
    .bind("b".repeat(64))
    .bind(vec![3u8; 800])
    .bind(vec![7u8; 24])
    .execute(library.db.pool())
    .await
    .unwrap();
    let run_id = uuid::Uuid::now_v7().to_string();
    sqlx::query(
        "INSERT INTO diarization_runs
             (id, recording_id, media_asset_id, provider_id, privacy_class, status, input_sha256,
              created_at)
         VALUES (?1, ?2, ?3, 'local', 'local_only', 'succeeded', ?4, 1)",
    )
    .bind(&run_id)
    .bind(library.recording_id.to_string())
    .bind(library.original_asset.to_string())
    .bind("c".repeat(64))
    .execute(library.db.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO speaker_suggestions
             (id, recording_id, diarization_run_id, cluster_key, person_id, score, score_kind,
              band, state, model_id, model_version, created_at)
         VALUES ('s1', ?1, ?2, 'c0', ?3, 0.84, 'cosine_similarity_asnorm', 'suggest', 'pending',
                 'wespeaker-resnet34-lm', '1.0', 1)",
    )
    .bind(library.recording_id.to_string())
    .bind(&run_id)
    .bind(person)
    .execute(library.db.pool())
    .await
    .unwrap();

    let service = DeletionService::new(library.db.clone(), library.archive.clone());
    let disclosure = service.disclose(library.recording_id).await.unwrap();
    assert_eq!(disclosure.counts.speaker_suggestions, 1);
    assert_eq!(disclosure.counts.voice_exemplars_provenance_cleared, 1);
    assert!(
        disclosure
            .limits
            .iter()
            .any(|line| line.contains("Voice profiles you enrolled are not deleted")),
        "the disclosure explains why the exemplar survives: {:?}",
        disclosure.limits
    );

    // The diarization run holds a RESTRICT reference, so it goes the way the
    // matching job's own cleanup would take it before the recording can go.
    sqlx::query("DELETE FROM diarization_runs WHERE id = ?1")
        .bind(&run_id)
        .execute(library.db.pool())
        .await
        .unwrap();
    let confirmation = confirm(&service, &library).await;
    service
        .delete(library.recording_id, &confirmation, NOW)
        .await
        .unwrap();

    let suggestions: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM speaker_suggestions")
        .fetch_one(library.db.pool())
        .await
        .unwrap();
    assert_eq!(suggestions, 0, "suggestions cascade with the recording");

    let row = sqlx::query(
        "SELECT recording_id, diarization_run_id, cluster_key, source_start_ms, source_end_ms
         FROM voice_exemplars WHERE id = 'x1'",
    )
    .fetch_one(library.db.pool())
    .await
    .unwrap();
    assert!(row.get::<Option<String>, _>("recording_id").is_none());
    assert!(row.get::<Option<String>, _>("diarization_run_id").is_none());
    assert!(row.get::<Option<String>, _>("cluster_key").is_none());
    assert!(row.get::<Option<i64>, _>("source_start_ms").is_none());
    assert!(row.get::<Option<i64>, _>("source_end_ms").is_none());

    let enrolments: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM person_voice_enrollments")
        .fetch_one(library.db.pool())
        .await
        .unwrap();
    assert_eq!(enrolments, 1, "the enrolment the user made survives");
    assert_eq!(dangling_references(&library.db).await, Vec::<String>::new());
}
