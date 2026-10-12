//! The spec 15.2 export commands, as typed request/response pairs.
//!
//! `export_recording`, `create_portable_backup`, `inspect_restore_archive` and
//! `restore_portable_archive` are implemented here against validated inputs:
//! a destination is an operating-system-selected folder that this crate
//! canonicalizes, a file name is generated, and nothing in a request is ever
//! used as a path fragment. The Tauri layer binds these one to one.

use std::path::{Path, PathBuf};

use plaudon_domain::ids::{MediaAssetId, RecordingId, TranscriptRevisionId};
use plaudon_domain::time::Timestamp;
use plaudon_persistence::archive::MediaArchive;
use plaudon_persistence::backups::{
    ArchiveBackupRecord, ArchiveDirection, ArchiveProtection, ArchiveVerifyStatus, ExportDerivative,
    ExportRepository,
};
use plaudon_persistence::Database;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::archive::crypto::KdfProfile;
use crate::archive::documents::{
    ANNOTATION_SCHEMA, OUTPUT_SCHEMA, RECORD_SCHEMA, SOURCE_SCHEMA, TOOL_SCHEMA, TRANSCRIPT_SCHEMA,
};
use crate::archive::manifest::ARCHIVE_EXTENSION;
use crate::archive::reader::InspectionReport;
use crate::archive::restore::{RestoreOptions, RestorePreview, RestoreReport, Restorer};
use crate::archive::snapshot::LibrarySnapshot;
use crate::archive::writer::{ArchiveBuilder, Protection};
use crate::bundle::BundleLoader;
use crate::captions::{self, CaptionFormat};
use crate::destination::{build_file_name, ExportDestination};
use crate::error::{ExportError, ExportResult};
use crate::media::{self, MediaExportReport};
use crate::render;

/// A text export format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportFormat {
    /// Plain text.
    Txt,
    /// Markdown.
    Markdown,
    /// Structured JSON.
    Json,
    /// SubRip captions.
    Srt,
    /// WebVTT captions.
    Vtt,
}

impl ExportFormat {
    /// File extension.
    #[must_use]
    pub const fn extension(self) -> &'static str {
        match self {
            Self::Txt => "txt",
            Self::Markdown => "md",
            Self::Json => "json",
            Self::Srt => "srt",
            Self::Vtt => "vtt",
        }
    }

    /// Stable wire string, also used for `export_events.format`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Txt => "txt",
            Self::Markdown => "markdown",
            Self::Json => "json",
            Self::Srt => "srt",
            Self::Vtt => "vtt",
        }
    }

    /// Every format, for a "export all" affordance.
    pub const ALL: &'static [Self] = &[Self::Txt, Self::Markdown, Self::Json, Self::Srt, Self::Vtt];
}

/// Request for `export_recording`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportRecordingRequest {
    /// Recording to export.
    pub recording_id: RecordingId,
    /// Revision to render. Defaults to the newest.
    pub transcript_revision_id: Option<TranscriptRevisionId>,
    /// Formats to produce.
    pub formats: Vec<ExportFormat>,
    /// Operating-system-selected destination folder.
    pub destination: PathBuf,
}

/// One produced file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportedFile {
    /// Format that produced it.
    pub format: ExportFormat,
    /// Where it landed.
    pub path: PathBuf,
    /// Size in bytes.
    pub byte_length: u64,
}

/// One format that was refused, with the reason.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportRefusal {
    /// Format that was not produced.
    pub format: ExportFormat,
    /// Stable application error code.
    pub code: String,
    /// User-facing explanation.
    pub explanation: String,
}

/// Response for `export_recording`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportRecordingResponse {
    /// Files written.
    pub files: Vec<ExportedFile>,
    /// Formats that were refused and why.
    pub refusals: Vec<ExportRefusal>,
    /// Revision the text renderings used.
    pub transcript_revision_id: Option<TranscriptRevisionId>,
}

/// Request for exporting audio.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportMediaRequest {
    /// Recording whose audio to export.
    pub recording_id: RecordingId,
    /// Candidate to export. Defaults to the preferred one.
    pub media_asset_id: Option<MediaAssetId>,
    /// Operating-system-selected destination folder.
    pub destination: PathBuf,
    /// Also produce a sanitized derivative, on explicit request (spec 7.8).
    pub include_sanitized_derivative: bool,
}

/// Response for exporting audio.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportMediaResponse {
    /// The exported original.
    pub original: MediaExportReport,
    /// The sanitized derivative, when one was requested and possible.
    pub sanitized: Option<MediaExportReport>,
    /// Why no derivative was produced, when one was requested.
    pub sanitized_refusal: Option<String>,
}

/// Request for `create_portable_backup`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupRequest {
    /// Operating-system-selected destination folder.
    pub destination: PathBuf,
    /// Password. When absent the archive is plaintext and says so.
    pub password: Option<String>,
    /// Whether to use the heavier Argon2id profile.
    #[serde(default)]
    pub sensitive_profile: bool,
}

/// Response for `create_portable_backup`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupResponse {
    /// Where the archive landed.
    pub path: PathBuf,
    /// Whether the body is encrypted.
    pub encrypted: bool,
    /// The sentence the UI must show about protection (spec 13.6).
    pub protection_disclosure: String,
    /// Number of entries, excluding the manifest.
    pub entry_count: u64,
    /// Sum of entry byte lengths.
    pub total_byte_length: u64,
    /// Archive file size.
    pub file_byte_length: u64,
    /// Manifest integrity digest.
    pub integrity_digest: String,
    /// What the digest proves.
    pub integrity_note: String,
    /// Manifest schema version.
    pub manifest_schema_version: String,
    /// Settings that do not travel with the archive.
    pub nonportable_settings: Vec<String>,
}

/// Request for `inspect_restore_archive`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InspectArchiveRequest {
    /// Operating-system-selected archive file.
    pub archive_path: PathBuf,
    /// Password, when the archive is encrypted.
    pub password: Option<String>,
}

/// Request for `restore_portable_archive`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestoreArchiveRequest {
    /// Operating-system-selected archive file.
    pub archive_path: PathBuf,
    /// Password, when the archive is encrypted.
    pub password: Option<String>,
    /// Restore options.
    #[serde(default)]
    pub options: RestoreOptions,
}

/// Export use cases over one library.
#[derive(Debug, Clone)]
pub struct ExportService {
    db: Database,
    media_archive: MediaArchive,
    library_root: PathBuf,
}

impl ExportService {
    /// Wrap a library.
    #[must_use]
    pub fn new(db: Database, media_archive: MediaArchive, library_root: impl Into<PathBuf>) -> Self {
        Self {
            db,
            media_archive,
            library_root: library_root.into(),
        }
    }

    /// `export_recording`: write the requested formats into a chosen folder.
    ///
    /// A format that cannot be produced honestly — captions for a transcript
    /// with no real timestamps — is refused individually and reported; the
    /// other formats still land.
    pub async fn export_recording(
        &self,
        request: &ExportRecordingRequest,
        now: Timestamp,
    ) -> ExportResult<ExportRecordingResponse> {
        let destination = ExportDestination::new(&request.destination)?;
        let loader = BundleLoader::new(self.db.clone());
        let export = loader
            .load(request.recording_id, request.transcript_revision_id, now)
            .await?;
        let repository = ExportRepository::new(self.db.clone());

        let discriminator = short_id(&request.recording_id.to_string());
        let mut files = Vec::new();
        let mut refusals = Vec::new();

        for format in &request.formats {
            let rendered = match format {
                ExportFormat::Txt => Ok(render::to_text(&export)),
                ExportFormat::Markdown => Ok(render::to_markdown(&export)),
                ExportFormat::Json => render::to_json(&export),
                ExportFormat::Srt | ExportFormat::Vtt => {
                    let caption_format = if *format == ExportFormat::Srt {
                        CaptionFormat::Srt
                    } else {
                        CaptionFormat::Vtt
                    };
                    match export.selected_transcript() {
                        Some(transcript) => captions::render(&export, transcript, caption_format),
                        None => Err(ExportError::CaptionsUnavailable(
                            crate::error::CaptionRefusal::NoSegments,
                        )),
                    }
                }
            };

            match rendered {
                Ok(text) => {
                    let name = build_file_name(
                        &export.recording.title,
                        &discriminator,
                        format.extension(),
                    );
                    let path = destination.write_atomic(&name, text.as_bytes())?;
                    repository
                        .record_export(
                            Some(request.recording_id),
                            format.as_str(),
                            Some(&destination.hint()),
                            now,
                        )
                        .await?;
                    files.push(ExportedFile {
                        format: *format,
                        path,
                        byte_length: text.len() as u64,
                    });
                }
                Err(error @ ExportError::CaptionsUnavailable(_)) => {
                    refusals.push(ExportRefusal {
                        format: *format,
                        code: error.code().as_str().to_owned(),
                        explanation: error.to_string(),
                    });
                }
                Err(other) => return Err(other),
            }
        }

        Ok(ExportRecordingResponse {
            files,
            refusals,
            transcript_revision_id: export.selected_transcript_id,
        })
    }

    /// Export the original audio, and optionally a sanitized derivative.
    pub async fn export_media(
        &self,
        request: &ExportMediaRequest,
        now: Timestamp,
    ) -> ExportResult<ExportMediaResponse> {
        let destination = ExportDestination::new(&request.destination)?;
        let loader = BundleLoader::new(self.db.clone());
        let export = loader.load(request.recording_id, None, now).await?;
        let repository = ExportRepository::new(self.db.clone());

        let wanted = request
            .media_asset_id
            .or(export.recording.preferred_media_asset_id);
        let facts = match wanted {
            Some(id) => export
                .media
                .iter()
                .find(|media| media.id == id)
                .ok_or(ExportError::RecordNotFound)?,
            None => export.media.first().ok_or(ExportError::RecordNotFound)?,
        };

        let snapshot = LibrarySnapshot::new(self.db.clone());
        let relative = snapshot.media_relative_path(&facts.id.to_string()).await?;
        let source = self.media_archive.absolute(&relative)?;
        let extension = crate::archive::snapshot::extension_of(&relative);
        let identity = plaudon_domain::media::MediaIdentity::new(
            facts
                .sha256
                .parse()
                .map_err(|_| ExportError::IntegrityFailed)?,
            facts.byte_length,
            facts.codec.clone().unwrap_or_default(),
        );

        let discriminator = short_id(&request.recording_id.to_string());
        let original = media::export_original(
            &source,
            &identity,
            &destination,
            &build_file_name(&export.recording.title, &discriminator, &extension),
            &facts.quality_label,
        )?;
        repository
            .record_export(
                Some(request.recording_id),
                "audio_original",
                Some(&destination.hint()),
                now,
            )
            .await?;

        let mut sanitized = None;
        let mut sanitized_refusal = None;
        if request.include_sanitized_derivative {
            let name = build_file_name(
                &format!("{} sanitized", export.recording.title),
                &discriminator,
                &extension,
            );
            match media::export_sanitized(&source, &destination, &name) {
                Ok(report) => {
                    repository
                        .record_derivative(&ExportDerivative {
                            id: Uuid::now_v7().to_string(),
                            recording_id: request.recording_id,
                            source_media_asset_id: facts.id,
                            identity: plaudon_domain::media::MediaIdentity::new(
                                report
                                    .sha256
                                    .parse()
                                    .map_err(|_| ExportError::IntegrityFailed)?,
                                report.byte_length,
                                String::new(),
                            ),
                            stripped_metadata: report.stripped_metadata.clone(),
                            created_at: now,
                        })
                        .await?;
                    repository
                        .record_export(
                            Some(request.recording_id),
                            "audio_sanitized_derivative",
                            Some(&destination.hint()),
                            now,
                        )
                        .await?;
                    sanitized = Some(report);
                }
                Err(error @ ExportError::SanitizationUnsupported(_)) => {
                    sanitized_refusal = Some(error.to_string());
                }
                Err(other) => return Err(other),
            }
        }

        Ok(ExportMediaResponse {
            original,
            sanitized,
            sanitized_refusal,
        })
    }

    /// `create_portable_backup`: a complete, restorable archive.
    pub async fn create_portable_backup(
        &self,
        request: &BackupRequest,
        now: Timestamp,
    ) -> ExportResult<BackupResponse> {
        let destination = ExportDestination::new(&request.destination)?;
        let snapshot = LibrarySnapshot::new(self.db.clone());

        let staging_base = self.library_root.join("temp");
        std::fs::create_dir_all(&staging_base)?;
        let mut builder = ArchiveBuilder::new(&staging_base, now)?;

        for source in snapshot.source_documents().await? {
            let document = serde_json::to_vec(&source)?;
            builder.add_document(&format!("sources/{}.json", source.id), SOURCE_SCHEMA, &document)?;
        }
        for tool in snapshot.tool_documents().await? {
            let document = serde_json::to_vec(&tool)?;
            builder.add_document(
                &format!("tools/{}-{}.json", tool.id, tool.version),
                TOOL_SCHEMA,
                &document,
            )?;
        }

        let mut media_seen: Vec<String> = Vec::new();
        for recording_id in snapshot.recording_ids().await? {
            let record = snapshot.record_document(&recording_id).await?;
            for asset in &record.media_assets {
                if media_seen.contains(&asset.sha256) {
                    continue;
                }
                let relative = snapshot.media_relative_path(&asset.id).await?;
                let source = self.media_archive.absolute(&relative)?;
                builder.add_media(
                    &format!("media/{}.{}", asset.sha256, asset.extension),
                    &source,
                    &asset.sha256,
                    asset.byte_length,
                )?;
                media_seen.push(asset.sha256.clone());
            }

            let document = serde_json::to_vec(&record)?;
            builder.add_document(
                &format!("records/{recording_id}.json"),
                RECORD_SCHEMA,
                &document,
            )?;

            let annotations = snapshot.annotation_document(&recording_id).await?;
            let document = serde_json::to_vec(&annotations)?;
            builder.add_document(
                &format!("annotations/{recording_id}.json"),
                ANNOTATION_SCHEMA,
                &document,
            )?;

            for transcript in snapshot.transcript_documents(&recording_id).await? {
                let document = serde_json::to_vec(&transcript)?;
                builder.add_document(
                    &format!("transcripts/{}.json", transcript.revision.id),
                    TRANSCRIPT_SCHEMA,
                    &document,
                )?;
            }
            for output in snapshot.output_documents(&recording_id).await? {
                let document = serde_json::to_vec(&output)?;
                builder.add_document(
                    &format!("outputs/{}.json", output.output.id),
                    OUTPUT_SCHEMA,
                    &document,
                )?;
            }
        }

        let protection = match &request.password {
            Some(password) => Protection::Encrypted {
                password: password.clone(),
                profile: if request.sensitive_profile {
                    KdfProfile::Sensitive
                } else {
                    KdfProfile::Backup
                },
            },
            None => Protection::Plaintext,
        };

        let stamp = now
            .to_rfc3339()
            .unwrap_or_else(|| now.as_millis().to_string())
            .replace(':', "-");
        let file_name = format!("plaudon-library-{stamp}.{ARCHIVE_EXTENSION}");
        let artifact = builder.finish(&destination, &file_name, &protection)?;

        let encrypted = artifact.protection == crate::archive::ProtectionKind::Encrypted;
        let stored_protection = if encrypted {
            ArchiveProtection::Encrypted
        } else {
            ArchiveProtection::Plaintext
        };

        ExportRepository::new(self.db.clone())
            .record_archive(&ArchiveBackupRecord {
                id: Uuid::now_v7().to_string(),
                direction: ArchiveDirection::Backup,
                manifest_schema_version: artifact.manifest.schema_version.clone(),
                format_version: u32::from(artifact.manifest.format_version),
                protection: stored_protection,
                location_hint: Some(destination.hint()),
                entry_count: artifact.manifest.totals.entry_count,
                total_byte_length: artifact.manifest.totals.byte_length,
                integrity_digest: artifact.manifest.integrity_digest.clone(),
                verify_status: ArchiveVerifyStatus::Verified,
                created_at: now,
            })
            .await?;

        Ok(BackupResponse {
            path: artifact.path,
            encrypted,
            protection_disclosure: stored_protection.disclosure().to_owned(),
            entry_count: artifact.manifest.totals.entry_count,
            total_byte_length: artifact.manifest.totals.byte_length,
            file_byte_length: artifact.file_bytes,
            integrity_digest: artifact.manifest.integrity_digest,
            integrity_note: artifact.manifest.integrity_note,
            manifest_schema_version: artifact.manifest.schema_version,
            nonportable_settings: artifact.manifest.nonportable_settings,
        })
    }

    /// `inspect_restore_archive`: validate and report, writing nothing.
    pub async fn inspect_restore_archive(
        &self,
        request: &InspectArchiveRequest,
    ) -> ExportResult<RestorePreview> {
        self.restorer()
            .dry_run(&request.archive_path, request.password.as_deref())
            .await
    }

    /// Inspect an archive without consulting the library at all.
    pub fn inspect_archive_only(
        archive_path: &Path,
        password: Option<&str>,
        probe_root: &Path,
    ) -> ExportResult<InspectionReport> {
        crate::archive::inspect(archive_path, password, probe_root)
    }

    /// `restore_portable_archive`: stage, verify, then integrate.
    pub async fn restore_portable_archive(
        &self,
        request: &RestoreArchiveRequest,
        now: Timestamp,
    ) -> ExportResult<RestoreReport> {
        let restorer = self.restorer();
        let (manifest, encrypted) = crate::archive::reader::read_manifest(
            &request.archive_path,
            request.password.as_deref(),
        )?;
        let report = restorer
            .restore(
                &request.archive_path,
                request.password.as_deref(),
                &request.options,
            )
            .await?;

        ExportRepository::new(self.db.clone())
            .record_archive(&ArchiveBackupRecord {
                id: Uuid::now_v7().to_string(),
                direction: ArchiveDirection::Restore,
                manifest_schema_version: manifest.schema_version.clone(),
                format_version: u32::from(manifest.format_version),
                protection: if encrypted {
                    ArchiveProtection::Encrypted
                } else {
                    ArchiveProtection::Plaintext
                },
                location_hint: request
                    .archive_path
                    .parent()
                    .and_then(Path::file_name)
                    .map(|name| name.to_string_lossy().into_owned()),
                entry_count: manifest.totals.entry_count,
                total_byte_length: manifest.totals.byte_length,
                integrity_digest: manifest.integrity_digest,
                verify_status: ArchiveVerifyStatus::Verified,
                created_at: now,
            })
            .await?;

        Ok(report)
    }

    fn restorer(&self) -> Restorer {
        Restorer::new(
            self.db.clone(),
            self.media_archive.clone(),
            self.library_root.clone(),
        )
    }
}

/// First eight characters of an identifier, for a readable file-name suffix.
fn short_id(id: &str) -> String {
    id.chars().take(8).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_have_distinct_extensions_and_stable_names() {
        let mut extensions: Vec<&str> = ExportFormat::ALL.iter().map(|f| f.extension()).collect();
        extensions.sort_unstable();
        extensions.dedup();
        assert_eq!(extensions.len(), ExportFormat::ALL.len());
        assert_eq!(ExportFormat::Markdown.as_str(), "markdown");
        assert_eq!(ExportFormat::Markdown.extension(), "md");
    }

    #[test]
    fn a_format_round_trips_through_its_wire_form() {
        for format in ExportFormat::ALL {
            let json = serde_json::to_string(format).unwrap();
            let back: ExportFormat = serde_json::from_str(&json).unwrap();
            assert_eq!(&back, format);
        }
    }
}
