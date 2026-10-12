//! Speaker labelling and confirmation (spec §8.7, §17.6).

use plaudon_domain::error::AppError;
use plaudon_domain::ids::{RecordingId, SpeakerId};
use plaudon_domain::time::Timestamp;

use crate::dto::{
    ConfirmSpeakerRequest, ListSpeakersRequest, ListSpeakersResponse, RenameSpeakerRequest,
    ResetSpeakerRequest, SpeakerDto, SpeakerOperationResponse,
};
use crate::validate::parse_id;
use crate::AppPorts;

/// List all speakers for a recording.
pub async fn list_speakers(
    ports: &AppPorts,
    request: ListSpeakersRequest,
) -> Result<ListSpeakersResponse, AppError> {
    let recording_id: RecordingId = parse_id("recording_id", &request.recording_id)?;
    let registry = ports.speakers.list_speakers(recording_id).await?;
    let speakers = registry
        .speakers()
        .iter()
        .map(|speaker| SpeakerDto {
            speaker_id: speaker.id,
            recording_id: speaker.recording_id,
            label: speaker.label.clone(),
            confirmed: speaker.user_confirmed,
            original_label: speaker.original_label.clone(),
            renamed_at: speaker.renamed_at,
            confirmed_at: speaker.confirmed_at,
        })
        .collect();
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
        speaker: SpeakerDto {
            speaker_id: speaker.id,
            recording_id: speaker.recording_id,
            label: speaker.label.clone(),
            confirmed: speaker.user_confirmed,
            original_label: speaker.original_label.clone(),
            renamed_at: speaker.renamed_at,
            confirmed_at: speaker.confirmed_at,
        },
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
        speaker: SpeakerDto {
            speaker_id: speaker.id,
            recording_id: speaker.recording_id,
            label: speaker.label.clone(),
            confirmed: speaker.user_confirmed,
            original_label: speaker.original_label.clone(),
            renamed_at: speaker.renamed_at,
            confirmed_at: speaker.confirmed_at,
        },
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
        speaker: SpeakerDto {
            speaker_id: speaker.id,
            recording_id: speaker.recording_id,
            label: speaker.label.clone(),
            confirmed: speaker.user_confirmed,
            original_label: speaker.original_label.clone(),
            renamed_at: speaker.renamed_at,
            confirmed_at: speaker.confirmed_at,
        },
    })
}
