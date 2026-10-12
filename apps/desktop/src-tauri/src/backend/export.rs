//! Transcript export (`export_recording`), run as a job.
//!
//! The destination is a path the user chose in the OS save dialog and that
//! [`crate::validate::validate_destination`] has already checked. We write a
//! sibling temporary file, fsync it and rename it onto exactly that path, so a
//! crash never leaves a half-written export behind.

use async_trait::async_trait;
use plaudon_domain::error::{AppError, AppErrorCode};
use plaudon_domain::ids::{JobId, RecordingId, TranscriptRevisionId};
use plaudon_domain::transcript::AlignmentOrigin;
use serde_json::json;

use crate::backend::store::storage_error;
use crate::backend::{jobs, LocalBackend};
use crate::dto::{EventKind, ExportFormat, JobKind, JobState, OutputLink, OutputLinkKind};
use crate::error::internal;
use crate::events::EventDraft;
use crate::ports::ExportPort;
use crate::validate::{validate_destination, ValidatedExport};
use plaudon_export::bundle::BundleLoader;
use plaudon_export::captions::{self, CaptionFormat};
use plaudon_export::error::{CaptionRefusal, ExportError};
use plaudon_export::render::{
    to_json as render_json, to_markdown as render_markdown, to_text as render_text,
};

#[async_trait]
impl ExportPort for LocalBackend {
    async fn export_recording(&self, request: ValidatedExport) -> Result<JobId, AppError> {
        let detail = self.recording_row(request.recording_id).await?;
        let revision = self.revision_ref(request.revision_id).await?;
        let owner = self.revision_recording(request.revision_id).await?;
        if owner != request.recording_id {
            return Err(AppError::new(
                AppErrorCode::RecordNotFound,
                "the revision does not belong to that recording",
            ));
        }

        let job_id = jobs::create(
            self.pool(),
            &jobs::JobDraft {
                kind: JobKind::Export,
                label: format!("Export {}", detail.title),
                recording_id: Some(request.recording_id),
                config_snapshot: json!({
                    "recording_id": request.recording_id.to_string(),
                    "revision_id": request.revision_id.to_string(),
                    "format": request.format,
                    "destination": request.destination.path.to_string_lossy(),
                }),
            },
        )
        .await?;

        let outcome = run(self, job_id, &request, revision.alignment_origin).await;
        finish(self, job_id, outcome).await?;
        Ok(job_id)
    }
}

/// Re-run an export from its durable config snapshot.
pub async fn rerun(backend: &LocalBackend, job_id: JobId) -> Result<(), AppError> {
    let config = jobs::config_snapshot(backend.pool(), job_id).await?;
    let recording_id: RecordingId = config
        .get("recording_id")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| internal("export job config has no recording id"))?
        .parse()
        .map_err(|_| internal("export job config recording id is not a uuid"))?;
    let revision_id: TranscriptRevisionId = config
        .get("revision_id")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| internal("export job config has no revision id"))?
        .parse()
        .map_err(|_| internal("export job config revision id is not a uuid"))?;
    let format: ExportFormat = config
        .get("format")
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .map_err(|err| internal(format!("export job format is invalid: {err}")))?
        .ok_or_else(|| internal("export job config has no format"))?;
    let destination = config
        .get("destination")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| internal("export job config has no destination"))?;

    let request = ValidatedExport {
        recording_id,
        revision_id,
        format,
        destination: validate_destination(destination)?,
    };
    let revision = backend.revision_ref(revision_id).await?;
    let outcome = run(backend, job_id, &request, revision.alignment_origin).await;
    finish(backend, job_id, outcome).await
}

async fn finish(
    backend: &LocalBackend,
    job_id: JobId,
    outcome: Result<(), AppError>,
) -> Result<(), AppError> {
    match outcome {
        Ok(()) => {
            if let Ok(summary) = jobs::load(backend.pool(), job_id).await {
                jobs::publish_update(backend.events(), &summary).await;
            }
            Ok(())
        }
        Err(error) => {
            jobs::fail(backend.pool(), job_id, &error).await?;
            if let Ok(summary) = jobs::load(backend.pool(), job_id).await {
                jobs::publish_update(backend.events(), &summary).await;
            }
            Err(error)
        }
    }
}

async fn run(
    backend: &LocalBackend,
    job_id: JobId,
    request: &ValidatedExport,
    alignment_origin: AlignmentOrigin,
) -> Result<(), AppError> {
    jobs::transition(
        backend.pool(),
        job_id,
        JobState::Preflight,
        Some("validating"),
        None,
    )
    .await?;
    jobs::transition(
        backend.pool(),
        job_id,
        JobState::RunningLocal,
        Some("rendering"),
        None,
    )
    .await?;
    let body = render(backend, request, alignment_origin).await?;

    write_and_verify(backend, job_id, &request.destination.path, &body).await?;
    commit_export(backend, job_id, request).await?;

    backend
        .events()
        .publish(
            EventDraft::new(
                EventKind::JobUpdated,
                plaudon_domain::time::Timestamp::now(),
            )
            .with_job(job_id)
            .with_recording(request.recording_id)
            .with_hint("state", json!(JobState::Completed.as_str())),
        )
        .await;
    Ok(())
}

/// Write the rendered document beside its destination, verify the bytes and
/// rename onto exactly the path the user chose.
async fn write_and_verify(
    backend: &LocalBackend,
    job_id: JobId,
    destination: &std::path::Path,
    body: &str,
) -> Result<(), AppError> {
    let parent = destination
        .parent()
        .ok_or_else(|| AppError::new(AppErrorCode::ArchivePathRejected, "no parent directory"))?;
    let temporary = parent.join(format!(".plaudon-export-{}.part", uuid::Uuid::new_v4()));
    tokio::fs::write(&temporary, body.as_bytes())
        .await
        .map_err(|err| {
            AppError::new(
                AppErrorCode::StorageUnavailable,
                format!("cannot write the export file: {err}"),
            )
        })?;

    jobs::transition(
        backend.pool(),
        job_id,
        JobState::Verifying,
        Some("verifying"),
        None,
    )
    .await?;
    let written = tokio::fs::read(&temporary).await.map_err(|err| {
        AppError::new(
            AppErrorCode::StorageUnavailable,
            format!("cannot read back the export file: {err}"),
        )
    })?;
    if written != body.as_bytes() {
        let _ = tokio::fs::remove_file(&temporary).await;
        return Err(AppError::new(
            AppErrorCode::IntegrityFailed,
            "the written export does not match the rendered document",
        ));
    }

    jobs::transition(
        backend.pool(),
        job_id,
        JobState::Committing,
        Some("committing"),
        None,
    )
    .await?;
    tokio::fs::rename(&temporary, destination)
        .await
        .map_err(|err| {
            AppError::new(
                AppErrorCode::StorageUnavailable,
                format!("cannot place the export file: {err}"),
            )
        })
}

/// Record the export event and complete the job in one transaction.
async fn commit_export(
    backend: &LocalBackend,
    job_id: JobId,
    request: &ValidatedExport,
) -> Result<(), AppError> {
    let mut tx = backend
        .pool()
        .begin()
        .await
        .map_err(|err| storage_error("cannot begin an export commit", &err))?;
    // Spec §13.2: `export_events` records that an export happened, with a
    // hint rather than the complete path, which may be sensitive.
    sqlx::query(
        "INSERT INTO export_events (id, recording_id, format, destination_hint, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
    )
    .bind(uuid::Uuid::now_v7().to_string())
    .bind(request.recording_id.to_string())
    .bind(format_str(request.format))
    .bind(
        request
            .destination
            .path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default(),
    )
    .bind(plaudon_domain::time::Timestamp::now().as_millis())
    .execute(&mut *tx)
    .await
    .map_err(|err| storage_error("cannot record an export event", &err))?;
    jobs::complete_in_tx(
        &mut tx,
        job_id,
        Some(OutputLink {
            kind: OutputLinkKind::Export,
            id: job_id.to_string(),
        }),
    )
    .await?;
    tx.commit()
        .await
        .map_err(|err| storage_error("cannot commit an export", &err))
}

/// Render a revision in the requested format, through `crates/export`.
///
/// Every rule lives there: segment text is copied verbatim, a `not_specified`
/// value is never filled in, an unconfirmed speaker name is never presented as
/// identity, and captions are refused outright when the revision has no real
/// timestamps (spec §4.6, §8.6A, §22(6)).
async fn render(
    backend: &LocalBackend,
    request: &ValidatedExport,
    alignment_origin: AlignmentOrigin,
) -> Result<String, AppError> {
    if request.format.needs_timestamps() && !alignment_origin.permits_times() {
        return Err(AppError::from(ExportError::CaptionsUnavailable(
            CaptionRefusal::NoAlignment,
        )));
    }
    let export = BundleLoader::new(backend.store().db().clone())
        .load(
            request.recording_id,
            Some(request.revision_id),
            plaudon_domain::time::Timestamp::now(),
        )
        .await?;
    let rendered = match request.format {
        ExportFormat::Txt => render_text(&export),
        ExportFormat::Markdown => render_markdown(&export),
        ExportFormat::Json => render_json(&export)?,
        ExportFormat::Srt | ExportFormat::Vtt => {
            let transcript = export.selected_transcript().ok_or_else(|| {
                AppError::from(ExportError::CaptionsUnavailable(CaptionRefusal::NoSegments))
            })?;
            let caption_format = if request.format == ExportFormat::Srt {
                CaptionFormat::Srt
            } else {
                CaptionFormat::Vtt
            };
            captions::render(&export, transcript, caption_format)?
        }
    };
    Ok(rendered)
}

const fn format_str(format: ExportFormat) -> &'static str {
    format.as_str()
}
