//! Manual import (spec §7.4) run as a real job over `plaudon_app_core`.
//!
//! The copy, hash, fsync, rename and journal steps belong to
//! `plaudon_persistence::journal` and `plaudon_app_core::import`; this module
//! only adds the job lifecycle
//! (`queued → preflight → running_local → verifying → committing → completed`,
//! or `failed_retryable` / `failed_terminal`), the duplicate decision the UI
//! asked for, and the activity entry committed with the job.
//!
//! Duplicates are decided on the **content hash only** — never on a file name
//! or a timestamp (spec §14.2). The source file is never modified, moved or
//! deleted.

use async_trait::async_trait;
use plaudon_app_core::import::{ImportOutcome as CoreOutcome, ImportRequest, ImportService};
use plaudon_domain::error::{AppError, AppErrorCode};
use plaudon_domain::ids::{JobId, RecordingId};
use plaudon_domain::media::{MediaIdentity, QualityClass, Sha256Digest};
use plaudon_domain::time::Timestamp;
use plaudon_notifications::privacy::NotificationContext;
use plaudon_persistence::archive::MediaArchive;
use serde_json::json;
use sqlx::Row;

use crate::backend::store::storage_error;
use crate::backend::{activity, jobs, LocalBackend};
use crate::dto::{
    ActivityTarget, ActivityTargetKind, DuplicatePolicy, EventKind, ImportFilesResponse,
    ImportOutcome, JobKind, JobState, NotificationCategory, OutputLink, OutputLinkKind,
};
use crate::error::envelope;
use crate::events::EventDraft;
use crate::ports::ImportPort;
use crate::validate::{import_paths, validate_import_path, ValidatedImportPath};

/// An existing recording that already holds the imported bytes.
#[derive(Debug, Clone)]
struct ExistingRecording {
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
            let label = std::path::Path::new(&path).file_name().map_or_else(
                || "Import".to_owned(),
                |name| name.to_string_lossy().into_owned(),
            );
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
pub async fn rerun(backend: &LocalBackend, job_id: JobId) -> Result<(), AppError> {
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
    job_id: JobId,
    requested_path: &str,
    policy: DuplicatePolicy,
) -> ImportOutcome {
    match run(backend, job_id, requested_path, policy).await {
        Ok(outcome) => outcome,
        Err(error) => {
            let rendered = match jobs::fail(backend.pool(), job_id, &error).await {
                Ok(rendered) => rendered,
                Err(secondary) => envelope(&secondary),
            };
            if let Ok(summary) = jobs::load(backend.pool(), job_id).await {
                jobs::publish_update(backend.events(), &summary).await;
            }
            ImportOutcome::Rejected {
                path: requested_path.to_owned(),
                error: rendered,
            }
        }
    }
}

async fn run(
    backend: &LocalBackend,
    job_id: JobId,
    requested_path: &str,
    policy: DuplicatePolicy,
) -> Result<ImportOutcome, AppError> {
    jobs::transition(
        backend.pool(),
        job_id,
        JobState::Preflight,
        Some("validating"),
        None,
    )
    .await?;
    let validated = validate_import_path(requested_path, backend.approved_roots())?;

    // Hash the source in place before copying anything: a duplicate the user
    // decides to skip must not cost a second copy of the bytes.
    jobs::transition(
        backend.pool(),
        job_id,
        JobState::RunningLocal,
        Some("hashing"),
        None,
    )
    .await?;
    let (digest, byte_length) =
        MediaArchive::digest_file(&validated.resolved).map_err(AppError::from)?;
    if byte_length == 0 {
        return Err(AppError::new(
            AppErrorCode::MediaValidationFailed,
            "the selected file is empty",
        ));
    }

    jobs::transition(
        backend.pool(),
        job_id,
        JobState::Verifying,
        Some("deduplicating"),
        None,
    )
    .await?;
    let existing = lookup_existing(backend, digest, byte_length).await?;

    if let Some(existing) = existing {
        match policy {
            DuplicatePolicy::Ask => {
                // A decision is owed by the user, which is exactly what
                // waiting_for_user means; nothing has been written.
                jobs::transition(
                    backend.pool(),
                    job_id,
                    JobState::WaitingForUser,
                    Some("duplicate_decision"),
                    None,
                )
                .await?;
                publish_job(backend, job_id).await;
                return Ok(ImportOutcome::Duplicate {
                    path: requested_path.to_owned(),
                    sha256: digest,
                    existing_recording_id: existing.recording_id,
                    existing_title: existing.title,
                });
            }
            DuplicatePolicy::Skip | DuplicatePolicy::OpenExisting => {
                jobs::transition(
                    backend.pool(),
                    job_id,
                    JobState::Cancelled,
                    Some("duplicate_skipped"),
                    None,
                )
                .await?;
                publish_job(backend, job_id).await;
                return Ok(ImportOutcome::Skipped {
                    path: requested_path.to_owned(),
                    sha256: digest,
                    existing_recording_id: existing.recording_id,
                });
            }
            // One blob, two logical recordings (spec §6.3B, §13.3A): the
            // import below reuses the stored blob instead of writing it twice.
            DuplicatePolicy::ImportAnyway => {}
        }
    }

    jobs::transition(
        backend.pool(),
        job_id,
        JobState::Committing,
        Some("committing"),
        None,
    )
    .await?;
    let (recording_id, activity_id) = commit(backend, &validated, job_id).await?;
    publish_import_events(backend, job_id, recording_id, &validated.original_file_name).await;
    // Strictly after the commit, and never awaited on the command path: the
    // OS notification is a side effect of a durable event (spec §14.5).
    backend
        .notifications()
        .notify(
            activity_id,
            NotificationContext::ImportFinished {
                imported: 1,
                failed: 0,
                recording: Some(recording_id),
            },
            Timestamp::now(),
        )
        .await;
    Ok(ImportOutcome::Accepted {
        path: requested_path.to_owned(),
        job_id,
    })
}

async fn lookup_existing(
    backend: &LocalBackend,
    digest: Sha256Digest,
    byte_length: u64,
) -> Result<Option<ExistingRecording>, AppError> {
    let identity = MediaIdentity::new(digest, byte_length, String::new());
    let Some(asset) = backend
        .assets()
        .find_by_identity(&identity)
        .await
        .map_err(AppError::from)?
    else {
        return Ok(None);
    };
    let row = sqlx::query(
        "SELECT r.id AS id, r.title AS title FROM recordings r \
         JOIN recording_media rm ON rm.recording_id = r.id \
         WHERE rm.media_asset_id = ?1 AND r.deleted_at IS NULL \
         ORDER BY rm.obtained_at ASC LIMIT 1",
    )
    .bind(asset.id.to_string())
    .fetch_optional(backend.pool())
    .await
    .map_err(|err| storage_error("cannot resolve a duplicate blob", &err))?;

    let Some(row) = row else { return Ok(None) };
    let id: String = row.get("id");
    Ok(Some(ExistingRecording {
        recording_id: id
            .parse()
            .map_err(|_| crate::error::internal("stored recording id is not a uuid"))?,
        title: row.get("title"),
    }))
}

/// Run the durable import and commit the job and activity entry with it.
async fn commit(
    backend: &LocalBackend,
    validated: &ValidatedImportPath,
    job_id: JobId,
) -> Result<(RecordingId, String), AppError> {
    let now = Timestamp::now();
    let title = title_for(validated);
    let mut request = ImportRequest::local_file(title.clone(), validated.extension.clone());
    request.quality_class = QualityClass::UserSuppliedFile;

    // `ImportService::import` streams from a `&mut dyn Read`, which is not
    // `Send`, so the whole use case runs on a blocking worker and the copy
    // never occupies an async worker thread either.
    let service = backend.imports().clone();
    let source = validated.resolved.clone();
    let outcome = tokio::task::spawn_blocking(move || {
        let mut file = std::fs::File::open(&source).map_err(|err| {
            AppError::new(
                AppErrorCode::ArchivePathRejected,
                format!("cannot open the source file: {err}"),
            )
        })?;
        tokio::runtime::Handle::current()
            .block_on(service.import(&request, &mut file, now))
            .map_err(AppError::from)
    })
    .await
    .map_err(|err| crate::error::internal(format!("the import worker failed: {err}")))??;

    let recording_id = match outcome {
        CoreOutcome::Imported { recording_id, .. }
        | CoreOutcome::MediaCandidateAdded { recording_id, .. }
        | CoreOutcome::AlreadyImported { recording_id, .. } => recording_id,
        CoreOutcome::RefusedTombstoned { .. } => {
            return Err(AppError::new(
                AppErrorCode::Conflict,
                "the recording was deleted locally and will not be resurrected",
            ))
        }
    };

    backend
        .record_original_file_name(recording_id, &validated.original_file_name, now)
        .await?;
    attach_recording(backend, job_id, recording_id).await?;

    let mut tx = plaudon_persistence::begin_write(backend.pool())
        .await
        .map_err(|err| storage_error("cannot begin an import commit", &err))?;
    jobs::complete_in_tx(
        &mut tx,
        job_id,
        Some(OutputLink {
            kind: OutputLinkKind::Recording,
            id: recording_id.to_string(),
        }),
    )
    .await?;
    // Spec §22(13): the activity entry is committed with the job, so it exists
    // whether or not a notification is ever delivered.
    let activity_id = activity::record_in_tx(
        &mut tx,
        &activity::ActivityDraft {
            category: NotificationCategory::ImportFinished,
            occurred_at: now,
            title: "Import finished".to_owned(),
            body: format!("{title} was added to your library."),
            target: Some(ActivityTarget {
                kind: ActivityTargetKind::Recording,
                id: recording_id.to_string(),
            }),
        },
    )
    .await?;
    tx.commit()
        .await
        .map_err(|err| storage_error("cannot commit an import", &err))?;

    Ok((recording_id, activity_id))
}

/// Record the produced recording on the job's durable config snapshot.
///
/// The core `jobs` table has no recording column; the snapshot is the row that
/// already carries the request, so the association lives there.
async fn attach_recording(
    backend: &LocalBackend,
    job_id: JobId,
    recording_id: RecordingId,
) -> Result<(), AppError> {
    let mut config = jobs::config_snapshot(backend.pool(), job_id).await?;
    if let Some(object) = config.as_object_mut() {
        object.insert("recording_id".to_owned(), json!(recording_id.to_string()));
    }
    sqlx::query("UPDATE jobs SET config_snapshot_json = ?2 WHERE id = ?1")
        .bind(job_id.to_string())
        .bind(config.to_string())
        .execute(backend.pool())
        .await
        .map_err(|err| storage_error("cannot link a job to its recording", &err))?;
    Ok(())
}

async fn publish_job(backend: &LocalBackend, job_id: JobId) {
    if let Ok(summary) = jobs::load(backend.pool(), job_id).await {
        jobs::publish_update(backend.events(), &summary).await;
    }
}

async fn publish_import_events(
    backend: &LocalBackend,
    job_id: JobId,
    recording_id: RecordingId,
    original_file_name: &str,
) {
    let now = Timestamp::now();
    publish_job(backend, job_id).await;
    backend
        .events()
        .publish(
            EventDraft::new(EventKind::RecordingImported, now)
                .with_job(job_id)
                .with_recording(recording_id)
                .with_hint("original_file_name", json!(original_file_name)),
        )
        .await;
    backend
        .events()
        .publish(EventDraft::new(EventKind::ActivityAdded, now).with_recording(recording_id))
        .await;
    backend
        .events()
        .publish(EventDraft::new(EventKind::LibraryChanged, now))
        .await;
}

/// Display title derived from the original file name.
///
/// The name is data: it is only stored and shown, never used to build a path
/// (archive paths come from the content hash) and never passed to a process.
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

/// The service a replacement implementation must provide.
///
/// Kept as a named helper so the import path has exactly one place that knows
/// which use case it drives.
#[must_use]
pub fn service(backend: &LocalBackend) -> &ImportService {
    backend.imports()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::testing::backend_fixture;
    use crate::ports::ImportPort;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_empty_file_is_rejected_before_anything_is_stored() {
        let fixture = backend_fixture().await;
        let path = fixture.source_file("empty.wav", b"");
        let response = fixture
            .backend
            .import_files(vec![path], DuplicatePolicy::Ask)
            .await
            .unwrap();
        let ImportOutcome::Rejected { error, .. } = &response.results[0] else {
            panic!("an empty file must be rejected");
        };
        assert_eq!(error.code, AppErrorCode::MediaValidationFailed);
        assert_eq!(fixture.backend.archive().list_blobs().unwrap().len(), 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_source_file_is_never_modified_or_removed() {
        let fixture = backend_fixture().await;
        let bytes = b"RIFF original bytes".to_vec();
        let path = fixture.source_file("keep.wav", &bytes);
        fixture
            .backend
            .import_files(vec![path.clone()], DuplicatePolicy::Ask)
            .await
            .unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_import_walks_the_expected_job_states() {
        let fixture = backend_fixture().await;
        let path = fixture.source_file("states.wav", b"RIFF states");
        let response = fixture
            .backend
            .import_files(vec![path], DuplicatePolicy::Ask)
            .await
            .unwrap();
        let ImportOutcome::Accepted { job_id, .. } = &response.results[0] else {
            panic!("expected an accepted import");
        };
        let summary = jobs::load(fixture.backend.pool(), *job_id).await.unwrap();
        assert_eq!(summary.state, JobState::Completed);
        assert_eq!(summary.kind, JobKind::Import);
        assert!(summary.finished_at.is_some());

        let steps: Vec<String> =
            sqlx::query_scalar("SELECT state FROM job_steps WHERE job_id = ?1 ORDER BY sequence")
                .bind(job_id.to_string())
                .fetch_all(fixture.backend.pool())
                .await
                .unwrap();
        assert_eq!(
            steps,
            vec![
                "queued".to_owned(),
                "preflight".to_owned(),
                "running_local".to_owned(),
                "verifying".to_owned(),
                "committing".to_owned(),
                "completed".to_owned(),
            ]
        );
    }
}
