//! Validated logic behind `get_transcript` and `save_transcript_revision`.

use plaudon_domain::error::AppError;
use plaudon_domain::ids::{RecordingId, TranscriptRevisionId};

use crate::dto::{
    GetTranscriptRequest, SaveTranscriptRevisionRequest, SaveTranscriptRevisionResponse,
    TranscriptPage,
};
use crate::validate::{
    limit, parse_id, segment_edits, ValidatedGetTranscript, ValidatedSaveRevision,
};
use crate::AppPorts;

/// One page of segments.
pub async fn get_transcript(
    ports: &AppPorts,
    request: GetTranscriptRequest,
) -> Result<TranscriptPage, AppError> {
    let query = ValidatedGetTranscript {
        revision_id: parse_id::<TranscriptRevisionId>("revision_id", &request.revision_id)?,
        cursor: request.cursor.unwrap_or(0),
        limit: limit(request.limit)?,
    };
    ports.transcript.get_transcript(query).await
}

/// Save edits as a new revision whose parent is the base.
pub async fn save_transcript_revision(
    ports: &AppPorts,
    request: SaveTranscriptRevisionRequest,
) -> Result<SaveTranscriptRevisionResponse, AppError> {
    let validated = ValidatedSaveRevision {
        recording_id: parse_id::<RecordingId>("recording_id", &request.recording_id)?,
        base_revision_id: parse_id::<TranscriptRevisionId>(
            "base_revision_id",
            &request.base_revision_id,
        )?,
        edits: segment_edits(&request.edits)?,
    };
    ports.transcript.save_transcript_revision(validated).await
}
