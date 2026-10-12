//! Audio export, deletion and the portable archive, behind the EPIC J crates.
//!
//! Nothing here re-implements a rule: `plaudon_export` owns rendering, the
//! container, encryption, verification and restore, and
//! `plaudon_app_core::deletion` owns the disclosure, the journal and the
//! tombstone. This module validates the request, calls them, and maps the
//! result onto the wire DTOs.

use async_trait::async_trait;
use plaudon_app_core::deletion::{DeletionConfirmation, DeletionService};
use plaudon_domain::error::AppError;
use plaudon_domain::ids::{MediaAssetId, RecordingId};
use plaudon_domain::time::Timestamp;
use plaudon_export::archive::restore::{ConflictPolicy, RestoreMode, RestoreOptions};
use plaudon_export::media::MediaExportReport;
use plaudon_export::service::{
    BackupRequest as ServiceBackupRequest, ExportMediaRequest as ServiceExportMediaRequest,
    ExportService, InspectArchiveRequest as ServiceInspectRequest,
    RestoreArchiveRequest as ServiceRestoreRequest,
};

use crate::backend::LocalBackend;
use crate::dto::{
    ConflictPolicyDto, CreateBackupResponse, DeleteRecordingResponse, DeletionCategoryDto,
    DeletionDisclosureDto, DeletionMediaDto, ExportMediaResponse, ExportedMediaFile,
    InspectArchiveResponse, RestoreArchiveResponse, RestoreConflictDto, RestoreModeDto,
};
use crate::ports::{ArchivePort, DeletionPort, MediaExportPort};
use crate::validate::{ValidatedArchiveFile, ValidatedDirectory};

impl LocalBackend {
    fn export_service(&self) -> ExportService {
        ExportService::new(
            self.store().db().clone(),
            self.store().archive().clone(),
            self.store().root().to_path_buf(),
        )
    }

    fn deletion_service(&self) -> DeletionService {
        DeletionService::new(self.store().db().clone(), self.store().archive().clone())
    }
}

fn media_file(report: &MediaExportReport) -> ExportedMediaFile {
    ExportedMediaFile {
        file_name: report
            .path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default(),
        sha256: report.sha256.clone(),
        byte_length: report.byte_length,
        is_original: report.is_original,
        label: report.label.clone(),
        stripped_metadata: report.stripped_metadata.clone(),
    }
}

#[async_trait]
impl MediaExportPort for LocalBackend {
    async fn export_media(
        &self,
        recording_id: RecordingId,
        media_asset_id: Option<MediaAssetId>,
        destination: ValidatedDirectory,
        include_sanitized_derivative: bool,
    ) -> Result<ExportMediaResponse, AppError> {
        let response = self
            .export_service()
            .export_media(
                &ServiceExportMediaRequest {
                    recording_id,
                    media_asset_id,
                    destination: destination.path,
                    include_sanitized_derivative,
                },
                Timestamp::now(),
            )
            .await?;
        Ok(ExportMediaResponse {
            original: media_file(&response.original),
            sanitized: response.sanitized.as_ref().map(media_file),
            sanitized_refusal: response.sanitized_refusal,
        })
    }
}

#[async_trait]
impl DeletionPort for LocalBackend {
    async fn deletion_disclosure(
        &self,
        recording_id: RecordingId,
    ) -> Result<DeletionDisclosureDto, AppError> {
        let disclosure = self.deletion_service().disclose(recording_id).await?;
        let counts = disclosure.counts;
        let categories = vec![
            category("Transcript revisions", counts.transcript_revisions),
            category("Transcript segments", counts.transcript_segments),
            category("Word timings", counts.transcript_words),
            category("Transcript quality flags", counts.transcript_annotations),
            category("Summaries and extracted items", counts.ai_outputs),
            category("Source anchors", counts.output_evidence),
            category("Speaker labels", counts.speakers),
            category("Tags", counts.tags),
            category("Source associations", counts.source_items),
            category("Media revisions", counts.media_revisions),
            category("Export history entries", counts.export_events),
            category("Sanitized derivative records", counts.export_derivatives),
            category("Activity history entries", counts.activity_events),
        ];
        Ok(DeletionDisclosureDto {
            confirmation_token: disclosure.digest(),
            disclosure_version: disclosure.disclosure_version.clone(),
            recording_id: disclosure.recording_id.to_string(),
            title: disclosure.title.clone(),
            has_annotation: disclosure.has_annotation,
            media: disclosure
                .media
                .iter()
                .map(|media| DeletionMediaDto {
                    sha256: media.sha256.clone(),
                    byte_length: media.byte_length,
                    quality_label: plaudon_export::bundle::quality_label(&media.quality_class)
                        .to_owned(),
                    shared_with_other_recordings: media.shared_with_other_recordings,
                    bytes_will_be_removed: media.bytes_will_be_removed,
                })
                .collect(),
            categories,
            bytes_to_free: disclosure.bytes_to_free,
            limits: disclosure.limits.clone(),
        })
    }

    async fn delete_recording(
        &self,
        recording_id: RecordingId,
        confirmation_token: String,
    ) -> Result<DeleteRecordingResponse, AppError> {
        let receipt = self
            .deletion_service()
            .delete(
                recording_id,
                &DeletionConfirmation {
                    disclosure_digest: confirmation_token,
                    reason: "user_requested".to_owned(),
                },
                Timestamp::now(),
            )
            .await?;
        Ok(DeleteRecordingResponse {
            deletion_id: receipt.deletion_id,
            blobs_removed: receipt.blobs_removed,
            blobs_retained_shared: receipt.blobs_retained_shared,
            bytes_freed: receipt.bytes_freed,
            tombstoned: receipt.tombstoned,
        })
    }
}

#[async_trait]
impl ArchivePort for LocalBackend {
    async fn create_portable_backup(
        &self,
        destination: ValidatedDirectory,
        password: Option<String>,
        sensitive_profile: bool,
    ) -> Result<CreateBackupResponse, AppError> {
        let response = self
            .export_service()
            .create_portable_backup(
                &ServiceBackupRequest {
                    destination: destination.path,
                    password,
                    sensitive_profile,
                },
                Timestamp::now(),
            )
            .await?;
        Ok(CreateBackupResponse {
            file_name: response
                .path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default(),
            encrypted: response.encrypted,
            protection_disclosure: response.protection_disclosure,
            entry_count: response.entry_count,
            total_byte_length: response.total_byte_length,
            file_byte_length: response.file_byte_length,
            integrity_digest: response.integrity_digest,
            integrity_note: response.integrity_note,
            manifest_schema_version: response.manifest_schema_version,
            nonportable_settings: response.nonportable_settings,
        })
    }

    async fn inspect_restore_archive(
        &self,
        archive: ValidatedArchiveFile,
        password: Option<String>,
    ) -> Result<InspectArchiveResponse, AppError> {
        let preview = self
            .export_service()
            .inspect_restore_archive(&ServiceInspectRequest {
                archive_path: archive.path,
                password,
            })
            .await?;
        let inspection = preview.inspection;
        Ok(InspectArchiveResponse {
            encrypted: inspection.encrypted,
            manifest_schema_version: inspection.manifest.schema_version.clone(),
            format_version: u32::from(inspection.manifest.format_version),
            created_at: inspection.manifest.created_at.clone(),
            source_os: inspection.manifest.source_os.clone(),
            entries_by_namespace: inspection
                .entries_by_namespace
                .iter()
                .map(|(name, count)| category(name, *count))
                .collect(),
            total_byte_length: inspection.manifest.totals.byte_length,
            payloads_verified: inspection.payloads_verified,
            integrity_digest: inspection.manifest.integrity_digest.clone(),
            integrity_note: inspection.manifest.integrity_note.clone(),
            nonportable_settings: inspection.nonportable_settings,
            conflicts: preview.conflicts.iter().map(conflict).collect(),
            target_is_empty: preview.target_is_empty,
        })
    }

    async fn restore_portable_archive(
        &self,
        archive: ValidatedArchiveFile,
        password: Option<String>,
        mode: RestoreModeDto,
        on_conflict: ConflictPolicyDto,
        restore_tombstoned: bool,
    ) -> Result<RestoreArchiveResponse, AppError> {
        let report = self
            .export_service()
            .restore_portable_archive(
                &ServiceRestoreRequest {
                    archive_path: archive.path,
                    password,
                    options: RestoreOptions {
                        mode: match mode {
                            RestoreModeDto::NewLibrary => RestoreMode::NewLibrary,
                            RestoreModeDto::Merge => RestoreMode::Merge,
                        },
                        on_conflict: match on_conflict {
                            ConflictPolicyDto::Skip => ConflictPolicy::Skip,
                            ConflictPolicyDto::Fail => ConflictPolicy::Fail,
                        },
                        restore_tombstoned,
                    },
                },
                Timestamp::now(),
            )
            .await?;
        Ok(RestoreArchiveResponse {
            recordings_restored: report.recordings_restored,
            transcripts_restored: report.transcripts_restored,
            outputs_restored: report.outputs_restored,
            sources_restored: report.sources_restored,
            media_committed: report.media_committed,
            media_reused: report.media_reused,
            conflicts: report.conflicts.iter().map(conflict).collect(),
        })
    }
}

fn category(label: &str, count: u64) -> DeletionCategoryDto {
    DeletionCategoryDto {
        label: label.to_owned(),
        count,
    }
}

fn conflict(conflict: &plaudon_export::archive::RestoreConflict) -> RestoreConflictDto {
    RestoreConflictDto {
        entry_path: conflict.entry_path.clone(),
        reason: conflict.reason.clone(),
    }
}
