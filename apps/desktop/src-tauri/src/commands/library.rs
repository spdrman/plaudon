//! Validated logic behind `list_recordings` and `get_recording`.

use plaudon_domain::error::AppError;
use plaudon_domain::ids::RecordingId;

use crate::dto::{GetRecordingRequest, ListRecordingsRequest, RecordingDetail, RecordingPage};
use crate::validate::{
    cursor, date_bounds, limit, parse_id, source_kinds, ValidatedListRecordings,
    ValidatedRecordingFilter,
};
use crate::AppPorts;

/// One page of the library.
pub async fn list_recordings(
    ports: &AppPorts,
    request: ListRecordingsRequest,
) -> Result<RecordingPage, AppError> {
    let (date_from, date_to) = date_bounds(
        request.filter.date_from.as_deref(),
        request.filter.date_to.as_deref(),
    )?;
    let query = ValidatedListRecordings {
        cursor: cursor(request.cursor.as_deref())?,
        limit: limit(request.limit)?,
        filter: ValidatedRecordingFilter {
            query: request
                .filter
                .query
                .filter(|value| !value.trim().is_empty()),
            import_status: request.filter.import_status,
            transcription_status: request.filter.transcription_status,
            sources: source_kinds(&request.filter.sources)?,
            tags: request.filter.tags,
            date_from,
            date_to,
        },
        sort: request.sort,
    };
    ports.library.list_recordings(query).await
}

/// Full detail of one recording.
pub async fn get_recording(
    ports: &AppPorts,
    request: GetRecordingRequest,
) -> Result<RecordingDetail, AppError> {
    let recording_id: RecordingId = parse_id("recording_id", &request.recording_id)?;
    ports.library.get_recording(recording_id).await
}
