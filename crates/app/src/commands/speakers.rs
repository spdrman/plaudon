//! Speaker labelling and confirmation (spec §8.7, §17.6).
//!
//! Every answer here is the row the store wrote, mapped by one function, so
//! a caller that patches its list from a rename sees exactly what a later
//! `list_speakers` would tell it.

use plaudon_app_core::speakers::Speaker;
use plaudon_domain::error::AppError;
use plaudon_domain::ids::{RecordingId, SpeakerId, TranscriptRevisionId};
use plaudon_domain::time::Timestamp;

use crate::dto::{
    ConfirmSpeakerRequest, ListSpeakersRequest, ListSpeakersResponse, RenameSpeakerRequest,
    ResetSpeakerRequest, SetSegmentSpeakerRequest, SetSegmentSpeakerResponse, SpeakerDto,
    SpeakerOperationResponse,
};
use crate::validate::parse_id;
use crate::AppPorts;

/// One stored row as the wire describes it.
fn dto(speaker: &Speaker) -> SpeakerDto {
    SpeakerDto {
        speaker_id: speaker.id,
        recording_id: speaker.recording_id,
        cluster_key: speaker
            .cluster_key
            .as_ref()
            .map(|key| key.as_str().to_owned()),
        person_id: speaker.person_id,
        label: speaker.label.clone(),
        name: speaker.name.clone(),
        confirmed: speaker.user_confirmed,
        original_label: speaker.original_label.clone(),
        renamed_at: speaker.renamed_at,
        confirmed_at: speaker.confirmed_at,
    }
}

/// List all speakers for a recording.
pub async fn list_speakers(
    ports: &AppPorts,
    request: ListSpeakersRequest,
) -> Result<ListSpeakersResponse, AppError> {
    let recording_id: RecordingId = parse_id("recording_id", &request.recording_id)?;
    let registry = ports.speakers.list_speakers(recording_id).await?;
    let speakers = registry.speakers().iter().map(dto).collect();
    Ok(ListSpeakersResponse { speakers })
}

/// Rename a speaker cluster.
pub async fn rename_speaker(
    ports: &AppPorts,
    request: RenameSpeakerRequest,
) -> Result<SpeakerOperationResponse, AppError> {
    let recording_id: RecordingId = parse_id("recording_id", &request.recording_id)?;
    let speaker_id: SpeakerId = parse_id("speaker_id", &request.speaker_id)?;
    let speaker = ports
        .speakers
        .rename_speaker(recording_id, speaker_id, &request.name, Timestamp::now())
        .await?;
    Ok(SpeakerOperationResponse {
        speaker: dto(&speaker),
    })
}

/// Confirm a speaker's identity.
pub async fn confirm_speaker(
    ports: &AppPorts,
    request: ConfirmSpeakerRequest,
) -> Result<SpeakerOperationResponse, AppError> {
    let recording_id: RecordingId = parse_id("recording_id", &request.recording_id)?;
    let speaker_id: SpeakerId = parse_id("speaker_id", &request.speaker_id)?;
    let speaker = ports
        .speakers
        .confirm_speaker(recording_id, speaker_id, Timestamp::now())
        .await?;
    Ok(SpeakerOperationResponse {
        speaker: dto(&speaker),
    })
}

/// Reset a speaker to the machine-assigned label.
pub async fn reset_speaker(
    ports: &AppPorts,
    request: ResetSpeakerRequest,
) -> Result<SpeakerOperationResponse, AppError> {
    let recording_id: RecordingId = parse_id("recording_id", &request.recording_id)?;
    let speaker_id: SpeakerId = parse_id("speaker_id", &request.speaker_id)?;
    let speaker = ports
        .speakers
        .reset_speaker(recording_id, speaker_id)
        .await?;
    Ok(SpeakerOperationResponse {
        speaker: dto(&speaker),
    })
}

/// Attribute one segment to a speaker, or to nobody (spec §4.1, VP-031).
///
/// This is a user's decision being recorded, not an inference: the command
/// writes the row the user picked and never derives one from audio. A
/// `speaker_id` of `None` clears the attribution, which is the honest answer
/// when a segment is two people at once or nobody identifiable.
pub async fn set_segment_speaker(
    ports: &AppPorts,
    request: SetSegmentSpeakerRequest,
) -> Result<SetSegmentSpeakerResponse, AppError> {
    let recording_id: RecordingId = parse_id("recording_id", &request.recording_id)?;
    let revision_id: TranscriptRevisionId =
        parse_id("transcript_revision_id", &request.transcript_revision_id)?;
    let speaker_id: Option<SpeakerId> = request
        .speaker_id
        .as_deref()
        .map(|value| parse_id("speaker_id", value))
        .transpose()?;

    let assignment = ports
        .speakers
        .set_segment_speaker(recording_id, revision_id, request.seq_index, speaker_id)
        .await?;

    Ok(SetSegmentSpeakerResponse {
        recording_id,
        transcript_revision_id: revision_id,
        seq_index: request.seq_index,
        speaker_id: assignment.speaker_id,
        speaker_label: assignment.speaker_label,
    })
}
