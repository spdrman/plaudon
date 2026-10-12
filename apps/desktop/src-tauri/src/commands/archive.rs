//! Validated logic behind the audio-export, deletion and portable-archive
//! commands (spec §4.6, §7.10, §13.6, §13.8, §15.1).
//!
//! Every path in a request is a folder or file the user chose in an OS
//! dialog, re-validated here before it reaches the backend, and no request
//! field is ever used as a path fragment.

use plaudon_domain::error::AppError;
use plaudon_domain::ids::{MediaAssetId, RecordingId};

use crate::dto::{
    CreateBackupRequest, CreateBackupResponse, DeleteRecordingRequest, DeleteRecordingResponse,
    DeletionDisclosureDto, DeletionDisclosureRequest, ExportMediaRequest, ExportMediaResponse,
    InspectArchiveRequest, InspectArchiveResponse, RestoreArchiveRequest, RestoreArchiveResponse,
};
use crate::error::invalid_argument;
use crate::validate::{parse_id, validate_archive_file, validate_directory, validate_password};
use crate::AppPorts;

/// Longest confirmation token we accept: a lowercase SHA-256 in hex.
const CONFIRMATION_TOKEN_LEN: usize = 64;

/// `export_media`: copy the original audio, optionally with a sanitized
/// derivative.
pub async fn export_media(
    ports: &AppPorts,
    request: ExportMediaRequest,
) -> Result<ExportMediaResponse, AppError> {
    let recording_id: RecordingId = parse_id("recording_id", &request.recording_id)?;
    let media_asset_id: Option<MediaAssetId> = request
        .media_asset_id
        .as_deref()
        .map(|value| parse_id("media_asset_id", value))
        .transpose()?;
    let destination = validate_directory(&request.destination_directory)?;
    ports
        .media_export
        .export_media(
            recording_id,
            media_asset_id,
            destination,
            request.include_sanitized_derivative,
        )
        .await
}

/// `get_deletion_disclosure`: exactly what a deletion would remove.
pub async fn get_deletion_disclosure(
    ports: &AppPorts,
    request: DeletionDisclosureRequest,
) -> Result<DeletionDisclosureDto, AppError> {
    let recording_id: RecordingId = parse_id("recording_id", &request.recording_id)?;
    ports.deletion.deletion_disclosure(recording_id).await
}

/// `delete_recording`: delete after the user confirmed the exact disclosure.
pub async fn delete_recording(
    ports: &AppPorts,
    request: DeleteRecordingRequest,
) -> Result<DeleteRecordingResponse, AppError> {
    let recording_id: RecordingId = parse_id("recording_id", &request.recording_id)?;
    let token = request.confirmation_token.trim().to_ascii_lowercase();
    if token.len() != CONFIRMATION_TOKEN_LEN || !token.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(invalid_argument(
            "confirmation_token must be the token from the disclosure that was shown",
        ));
    }
    ports.deletion.delete_recording(recording_id, token).await
}

/// `create_portable_backup`: a complete, restorable archive.
pub async fn create_portable_backup(
    ports: &AppPorts,
    request: CreateBackupRequest,
) -> Result<CreateBackupResponse, AppError> {
    let destination = validate_directory(&request.destination_directory)?;
    let password = validate_password(request.password.as_deref())?;
    ports
        .archive
        .create_portable_backup(destination, password, request.sensitive_profile)
        .await
}

/// `inspect_restore_archive`: validate and report, writing nothing.
pub async fn inspect_restore_archive(
    ports: &AppPorts,
    request: InspectArchiveRequest,
) -> Result<InspectArchiveResponse, AppError> {
    let archive = validate_archive_file(&request.archive_path)?;
    let password = validate_password(request.password.as_deref())?;
    ports
        .archive
        .inspect_restore_archive(archive, password)
        .await
}

/// `restore_portable_archive`: stage, verify, then integrate.
pub async fn restore_portable_archive(
    ports: &AppPorts,
    request: RestoreArchiveRequest,
) -> Result<RestoreArchiveResponse, AppError> {
    let archive = validate_archive_file(&request.archive_path)?;
    let password = validate_password(request.password.as_deref())?;
    ports
        .archive
        .restore_portable_archive(
            archive,
            password,
            request.mode,
            request.on_conflict,
            request.restore_tombstoned,
        )
        .await
}
