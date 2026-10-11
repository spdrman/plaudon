//! Manual import (spec §7.4) run as a real job.
//!
//! One job per path, moving through
//! `queued → preflight → running_local → verifying → committing → completed`,
//! or to `failed_retryable` / `failed_terminal` with the envelope persisted on
//! the row. Duplicates are decided on the **content hash only** — never on a
//! file name or a timestamp (spec §14.2).

use async_trait::async_trait;
use plaudon_domain::error::{AppError, AppErrorCode};
use plaudon_domain::ids::{MediaAssetId, RecordingId};
use plaudon_domain::media::Sha256Digest;
use plaudon_domain::time::Timestamp;
use serde_json::json;
use sqlx::Row;

use crate::backend::archive::{digest_file, Archive, StagedFile};
use crate::backend::store::storage_error;
use crate::backend::{activity, jobs, LocalBackend};
use crate::dto::{
    ActivityTarget, ActivityTargetKind, DuplicatePolicy, EventKind, ImportFilesResponse,
    ImportOutcome, ImportStatus, JobKind, JobState, NotificationCategory, OutputLink,
    OutputLinkKind, SourceKind, TranscriptionStatus,
};
use crate::error::envelope;
use crate::events::EventDraft;
use crate::ports::ImportPort;
use crate::validate::{import_paths, validate_import_path, ValidatedImportPath};

/// An existing asset that matches the imported bytes.
#[derive(Debug, Clone)]
struct ExistingBlob {
    media_asset_id: MediaAssetId,
    relative_path: String,
    recording_id: RecordingId,
    title: String,
}

#[async_trait]
impl ImportPort for LocalBackend {
    async fn import_files(
        &self,
        paths: Vec<String>,
        duplicate_policy: DuplicatePolicy,
    ) -> Result<ImportFilesResponse, AppError> {
        let paths = import_paths(&paths)?;
        let mut results = Vec::with_capacity(paths.len());
        for path in paths {
            let label = std::path::Path::new(&path)
                .file_name()
                .map_or_else(|| "Import".to_owned(), |name| name.to_string_lossy().into_owned());
            let job_id = jobs::create(
                self.pool(),
                &jobs::JobDraft {
                    kind: JobKind::Import,
                    label,
                    recording_id: None,
                    config_snapshot: json!({
                        "path": path,
                        "duplicate_policy": duplicate_policy,
                    }),
                },
            )
            .await?;
            results.push(execute(self, job_id, &path, duplicate_policy).await);
        }
        Ok(ImportFilesResponse { results })
    }
}

/// Re-run the import a job was opened for, from its durable config snapshot.
pub async fn rerun(backend: &LocalBackend, job_id: plaudon_domain::ids::JobId) -> Result<(), AppError> {
    let config = jobs::config_snapshot(backend.pool(), job_id).await?;
    let path = config
        .get("path")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| crate::error::internal("import job config has no path"))?
        .to_owned();
    let policy: DuplicatePolicy = config
        .get("duplicate_policy")
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .map_err(|err| crate::error::internal(format!("import job policy is invalid: {err}")))?
        .unwrap_or(DuplicatePolicy::Ask);
    execute(backend, job_id, &path, policy).await;
    Ok(())
}

async fn execute(
    backend: &LocalBackend,
    job_id: plaudon_domain::ids::JobId,
    requested_path: &str,
    policy: DuplicatePolicy,
) -> ImportOutcome {
    match run(backend, job_id, requested_path, policy).await {
        Ok(outcome) => outcome,
        Err(error) => {
            let envelope = match jobs::fail(backend.pool(), job_id, &error).await {
                Ok(envelope) => envelope,
                Err(secondary) => envelope(&secondary),
            };
            if let Ok(summary) = jobs::load(backend.pool(), job_id).await {
                jobs::publish_update(backend.events(), &summary).await;
            }
            ImportOutcome::Rejected { path: requested_path.to_owned(), error: envelope }
        }
    }
}

async fn run(
    backend: &LocalBackend,
    job_id: plaudon_domain::ids::JobId,
    requested_path: &str,
    policy: DuplicatePolicy,
) -> Result<ImportOutcome, AppError> {
    jobs::transition(backend.pool(), job_id, JobState::Preflight, Some("validating"), None).await?;
    let validated = validate_import_path(requested_path, backend.approved_roots())?;

    jobs::transition(backend.pool(), job_id, JobState::RunningLocal, Some("hashing"), None).await?;
    let staged = backend.archive().stage(&validated.resolved).await?;

    jobs::transition(backend.pool(), job_id, JobState::Verifying, Some("verifying"), None).await?;
    let (observed, observed_length) = digest_file(&staged.path).await?;
    if observed != staged.digest || observed_length != staged.byte_length {
        Archive::discard(staged).await;
        return Err(AppError::new(
            AppErrorCode::IntegrityFailed,
            "the staged copy does not match the digest computed while streaming",
        ));
    }

    let existing = lookup_existing(backend, staged.digest, staged.byte_length).await?;

    if let Some(existing) = existing {
        return resolve_duplicate(backend, job_id, requested_path, &validated, staged, existing, policy)
            .await;
    }

    jobs::transition(backend.pool(), job_id, JobState::Committing, Some("committing"), None)
        .await?;
    let relative_path = backend.archive().commit(&staged, &validated.extension).await?;
    let recording_id =
        commit_new_recording(backend, job_id, &validated, &staged, &relative_path).await?;

    publish_import_events(backend, job_id, recording_id, &validated).await;
    Ok(ImportOutcome::Accepted { path: requested_path.to_owned(), job_id })
}

async fn lookup_existing(
    backend: &LocalBackend,
    digest: Sha256Digest,
    byte_length: u64,
) -> Result<Option<ExistingBlob>, AppError> {
    let length = i64::try_from(byte_length)
        .map_err(|_| crate::error::internal("byte length does not fit in the database"))?;
    let row = sqlx::query(
        "SELECT a.id AS asset_id, a.relative_path AS relative_path, r.id AS recording_id, \
         r.title AS title FROM media_assets a \
         JOIN recording_media rm ON rm.media_asset_id = a.id \
         JOIN recordings r ON r.id = rm.recording_id \
         WHERE a.sha256 = ?1 AND a.byte_length = ?2 \
         ORDER BY rm.attached_at ASC LIMIT 1",
    )
    .bind(digest.to_hex())
    .bind(length)
    .fetch_optional(backend.pool())
    .await
    .map_err(|err| storage_error("cannot look up an existing blob", &err))?;

    let Some(row) = row else { return Ok(None) };
    let asset_id: String = row.get("asset_id");
    let recording_id: String = row.get("recording_id");
    Ok(Some(ExistingBlob {
        media_asset_id: asset_id
            .parse()
            .map_err(|_| crate::error::internal("stored media asset id is not a uuid"))?,
        relative_path: row.get("relative_path"),
        recording_id: recording_id
            .parse()
            .map_err(|_| crate::error::internal("stored recording id is not a uuid"))?,
        title: row.get("title"),
    }))
}

#[allow(clippy::too_many_arguments)]
async fn resolve_duplicate(
    backend: &LocalBackend,
    job_id: plaudon_domain::ids::JobId,
    requested_path: &str,
    validated: &ValidatedImportPath,
    staged: StagedFile,
    existing: ExistingBlob,
    policy: DuplicatePolicy,
) -> Result<ImportOutcome, AppError> {
    let digest = staged.digest;
    match policy {
        DuplicatePolicy::Ask => {
            Archive::discard(staged).await;
            // A decision is owed by the user, which is exactly what
            // waiting_for_user means; nothing was written.
            jobs::transition(
                backend.pool(),
                job_id,
                JobState::WaitingForUser,
                Some("duplicate_decision"),
                None,
            )
            .await?;
            if let Ok(summary) = jobs::load(backend.pool(), job_id).await {
                jobs::publish_update(backend.events(), &summary).await;
            }
            Ok(ImportOutcome::Duplicate {
                path: requested_path.to_owned(),
                sha256: digest,
                existing_recording_id: existing.recording_id,
                existing_title: existing.title,
            })
        }
        DuplicatePolicy::Skip | DuplicatePolicy::OpenExisting => {
            Archive::discard(staged).await;
            jobs::transition(
                backend.pool(),
                job_id,
                JobState::Cancelled,
                Some("duplicate_skipped"),
                None,
            )
            .await?;
            if let Ok(summary) = jobs::load(backend.pool(), job_id).await {
                jobs::publish_update(backend.events(), &summary).await;
            }
            Ok(ImportOutcome::Skipped {
                path: requested_path.to_owned(),
                sha256: digest,
                existing_recording_id: existing.recording_id,
            })
        }
        DuplicatePolicy::ImportAnyway => {
            // One blob, two logical recordings (spec §13.3A).
            Archive::discard(staged).await;
            jobs::transition(
                backend.pool(),
                job_id,
                JobState::Committing,
                Some("committing"),
                None,
            )
            .await?;
            let recording_id = commit_recording_for_existing_asset(
                backend,
                job_id,
                validated,
                digest,
                &existing,
            )
            .await?;
            publish_import_events(backend, job_id, recording_id, validated).await;
            Ok(ImportOutcome::Accepted { path: requested_path.to_owned(), job_id })
        }
    }
}

async fn commit_new_recording(
    backend: &LocalBackend,
    job_id: plaudon_domain::ids::JobId,
    validated: &ValidatedImportPath,
    staged: &StagedFile,
    relative_path: &str,
) -> Result<RecordingId, AppError> {
    let now = Timestamp::now();
    let asset_id = MediaAssetId::new();
    let recording_id = RecordingId::new();
    let byte_length = i64::try_from(staged.byte_length)
        .map_err(|_| crate::error::internal("byte length does not fit in the database"))?;

    let mut tx =
        backend.pool().begin().await.map_err(|err| storage_error("cannot begin an import", &err))?;

    sqlx::query(
        "INSERT INTO media_assets (id, sha256, byte_length, relative_path, observed_codec, \
         sample_rate_hz, channels, duration_ms, verification_state, verified_at, created_at) \
         VALUES (?1, ?2, ?3, ?4, 'unknown', NULL, NULL, NULL, 'verified', ?5, ?5)",
    )
    .bind(asset_id.to_string())
    .bind(staged.digest.to_hex())
    .bind(byte_length)
    .bind(relative_path)
    .bind(now.as_millis())
    .execute(&mut *tx)
    .await
    .map_err(|err| storage_error("cannot insert a media asset", &err))?;

    insert_recording(&mut tx, recording_id, validated, now).await?;

    sqlx::query(
        "INSERT INTO recording_media (recording_id, media_asset_id, quality_class, \
         is_preferred, attached_at) VALUES (?1, ?2, 'user_supplied_file', 1, ?3)",
    )
    .bind(recording_id.to_string())
    .bind(asset_id.to_string())
    .bind(now.as_millis())
    .execute(&mut *tx)
    .await
    .map_err(|err| storage_error("cannot attach media to a recording", &err))?;

    attach_job_and_activity(&mut tx, job_id, recording_id, validated, now).await?;

    tx.commit().await.map_err(|err| storage_error("cannot commit an import", &err))?;
    Ok(recording_id)
}

async fn commit_recording_for_existing_asset(
    backend: &LocalBackend,
    job_id: plaudon_domain::ids::JobId,
    validated: &ValidatedImportPath,
    _digest: Sha256Digest,
    existing: &ExistingBlob,
) -> Result<RecordingId, AppError> {
    let now = Timestamp::now();
    let recording_id = RecordingId::new();
    let mut tx = backend
        .pool()
        .begin()
        .await
        .map_err(|err| storage_error("cannot begin a duplicate import", &err))?;

    insert_recording(&mut tx, recording_id, validated, now).await?;
    sqlx::query(
        "INSERT INTO recording_media (recording_id, media_asset_id, quality_class, \
         is_preferred, attached_at) VALUES (?1, ?2, 'user_supplied_file', 1, ?3)",
    )
    .bind(recording_id.to_string())
    .bind(existing.media_asset_id.to_string())
    .bind(now.as_millis())
    .execute(&mut *tx)
    .await
    .map_err(|err| storage_error("cannot attach the shared media asset", &err))?;

    attach_job_and_activity(&mut tx, job_id, recording_id, validated, now).await?;
    tx.commit().await.map_err(|err| storage_error("cannot commit a duplicate import", &err))?;

    debug_assert!(backend.archive().contains(&existing.relative_path));
    Ok(recording_id)
}

async fn insert_recording(
    tx: &mut sqlx::SqliteConnection,
    recording_id: RecordingId,
    validated: &ValidatedImportPath,
    now: Timestamp,
) -> Result<(), AppError> {
    // `recorded_at` and `duration_ms` stay NULL: nothing here measured them,
    // and a file modification time is not a capture time (spec §8.6A).
    sqlx::query(
        "INSERT INTO recordings (id, title, recorded_at, imported_at, duration_ms, source_kind, \
         source_label, import_status, transcription_status, processing_engine, last_error_json, \
         tags_json, original_file_name, current_revision_id, deleted_at) \
         VALUES (?1, ?2, NULL, ?3, NULL, ?4, ?5, ?6, ?7, NULL, NULL, '[]', ?8, NULL, NULL)",
    )
    .bind(recording_id.to_string())
    .bind(title_for(validated))
    .bind(now.as_millis())
    .bind(serde_json::to_value(SourceKind::ManualImport).map_or_else(
        |_| "manual_import".to_owned(),
        |value| value.as_str().unwrap_or("manual_import").to_owned(),
    ))
    .bind("Manual import")
    .bind(serde_json::to_value(ImportStatus::Imported).map_or_else(
        |_| "imported".to_owned(),
        |value| value.as_str().unwrap_or("imported").to_owned(),
    ))
    .bind(serde_json::to_value(TranscriptionStatus::None).map_or_else(
        |_| "none".to_owned(),
        |value| value.as_str().unwrap_or("none").to_owned(),
    ))
    .bind(&validated.original_file_name)
    .execute(&mut *tx)
    .await
    .map_err(|err| storage_error("cannot insert a recording", &err))?;
    Ok(())
}

async fn attach_job_and_activity(
    tx: &mut sqlx::SqliteConnection,
    job_id: plaudon_domain::ids::JobId,
    recording_id: RecordingId,
    validated: &ValidatedImportPath,
    now: Timestamp,
) -> Result<(), AppError> {
    sqlx::query("UPDATE jobs SET recording_id = ?2 WHERE id = ?1")
        .bind(job_id.to_string())
        .bind(recording_id.to_string())
        .execute(&mut *tx)
        .await
        .map_err(|err| storage_error("cannot link a job to its recording", &err))?;

    jobs::complete_in_tx(
        tx,
        job_id,
        Some(OutputLink { kind: OutputLinkKind::Recording, id: recording_id.to_string() }),
    )
    .await?;

    // Spec §22(13): the activity entry is committed with the job, so it exists
    // whether or not a notification is ever delivered.
    activity::record_in_tx(
        tx,
        &activity::ActivityDraft {
            category: NotificationCategory::ImportFinished,
            occurred_at: now,
            title: "Import finished".to_owned(),
            body: format!("{} was added to your library.", title_for(validated)),
            target: Some(ActivityTarget {
                kind: ActivityTargetKind::Recording,
                id: recording_id.to_string(),
            }),
        },
    )
    .await?;
    Ok(())
}

async fn publish_import_events(
    backend: &LocalBackend,
    job_id: plaudon_domain::ids::JobId,
    recording_id: RecordingId,
    validated: &ValidatedImportPath,
) {
    let now = Timestamp::now();
    if let Ok(summary) = jobs::load(backend.pool(), job_id).await {
        jobs::publish_update(backend.events(), &summary).await;
    }
    backend
        .events()
        .publish(
            EventDraft::new(EventKind::RecordingImported, now)
                .with_job(job_id)
                .with_recording(recording_id)
                .with_hint("title", json!(title_for(validated))),
        )
        .await;
    backend
        .events()
        .publish(EventDraft::new(EventKind::ActivityAdded, now).with_recording(recording_id))
        .await;
    backend.events().publish(EventDraft::new(EventKind::LibraryChanged, now)).await;
}

/// Display title derived from the original file name.
///
/// The name is data: it is only ever stored and shown, never used to build a
/// path (the archive name comes from the content hash) and never passed to a
/// process.
fn title_for(validated: &ValidatedImportPath) -> String {
    let stem = std::path::Path::new(&validated.original_file_name)
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default();
    if stem.is_empty() {
        validated.original_file_name.clone()
    } else {
        stem
    }
}
