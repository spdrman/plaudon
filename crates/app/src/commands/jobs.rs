//! Validated logic behind `list_jobs`, `cancel_job`, `retry_job` and
//! `export_recording`.

use plaudon_domain::error::AppError;
use plaudon_domain::ids::{JobId, RecordingId, TranscriptRevisionId};

use crate::dto::{
    CancelJobRequest, ExportRecordingRequest, ExportRecordingResponse, JobPage, JobSummary,
    ListJobsRequest, RetryJobRequest,
};
use crate::validate::{
    cursor, limit, parse_id, unique_kinds, unique_states, validate_destination, ValidatedExport,
    ValidatedListJobs,
};
use crate::AppPorts;

/// One page of jobs plus the per-state counts.
pub async fn list_jobs(ports: &AppPorts, request: ListJobsRequest) -> Result<JobPage, AppError> {
    let query = ValidatedListJobs {
        cursor: cursor(request.cursor.as_deref())?,
        limit: limit(request.limit)?,
        states: unique_states(&request.states)?,
        kinds: unique_kinds(&request.kinds)?,
    };
    ports.jobs.list_jobs(query).await
}

/// Cancel a non-terminal job.
pub async fn cancel_job(
    ports: &AppPorts,
    request: CancelJobRequest,
) -> Result<JobSummary, AppError> {
    let job_id: JobId = parse_id("job_id", &request.job_id)?;
    ports.jobs.cancel_job(job_id).await
}

/// Retry a job, refusing a possibly-billed retry without an acknowledgement.
pub async fn retry_job(ports: &AppPorts, request: RetryJobRequest) -> Result<JobSummary, AppError> {
    let job_id: JobId = parse_id("job_id", &request.job_id)?;
    ports
        .jobs
        .retry_job(job_id, request.cost_acknowledged)
        .await
}

/// Export a transcript revision to a user-chosen destination.
pub async fn export_recording(
    ports: &AppPorts,
    request: ExportRecordingRequest,
) -> Result<ExportRecordingResponse, AppError> {
    let validated = ValidatedExport {
        recording_id: parse_id::<RecordingId>("recording_id", &request.recording_id)?,
        revision_id: parse_id::<TranscriptRevisionId>("revision_id", &request.revision_id)?,
        format: request.format,
        destination: validate_destination(&request.destination)?,
    };
    let job_id = ports.export.export_recording(validated).await?;
    Ok(ExportRecordingResponse { job_id })
}
