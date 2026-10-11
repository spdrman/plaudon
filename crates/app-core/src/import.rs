//! The import use case: stage, deduplicate, commit, associate.
//!
//! The ordering here is the part that matters. Blob-level deduplication
//! happens inside the journal commit, while record-level identity is decided
//! before anything is staged, so a repeated import of the same source item
//! never creates a second recording and two different items with identical
//! bytes never collapse into one.

use std::collections::BTreeSet;
use std::io::Read;

use plaudon_domain::error::{AppError, AppErrorCode};
use plaudon_domain::ids::{MediaAssetId, RecordingId, SourceId, SourceItemId};
use plaudon_domain::media::QualityClass;
use plaudon_domain::recording::{Recording, RecordingMedia};
use plaudon_domain::source::{
    ImportStatus, RemoteState, SourceAccountScope, SourceAssociation, SourceIdentityKey, SourceKind,
};
use plaudon_domain::time::Timestamp;
use plaudon_persistence::archive::MediaArchive;
use plaudon_persistence::db::Database;
use plaudon_persistence::error::{PersistenceError, PersistenceResult};
use plaudon_persistence::journal::ImportJournal;
use plaudon_persistence::repo::{MediaAssetRepository, RecordingRepository, SourceRepository};

/// Everything needed to import one item.
#[derive(Debug, Clone)]
pub struct ImportRequest {
    /// Source-scoped identity of the item.
    pub identity: SourceIdentityKey,
    /// Human-readable source name, used for the initial title only.
    pub display_name: String,
    /// Fidelity class of the acquired bytes.
    pub quality_class: QualityClass,
    /// Conventional extension of the sniffed container.
    pub extension: String,
    /// Codec observed by our decoder.
    pub observed_codec: String,
    /// Remote version hint, when the source exposes one.
    pub remote_version: Option<String>,
    /// Recording to attach the media to, when the caller is adding a
    /// higher-fidelity candidate to an existing meeting rather than importing
    /// a new one.
    pub attach_to_recording: Option<RecordingId>,
}

impl ImportRequest {
    /// A request for a user-selected local file.
    #[must_use]
    pub fn local_file(display_name: impl Into<String>, extension: impl Into<String>) -> Self {
        Self {
            identity: SourceIdentityKey::new(
                SourceKind::LocalFile,
                SourceAccountScope::new("local"),
                None,
            ),
            display_name: display_name.into(),
            quality_class: QualityClass::UserSuppliedFile,
            extension: extension.into(),
            observed_codec: String::new(),
            remote_version: None,
            attach_to_recording: None,
        }
    }
}

/// What an import actually did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportOutcome {
    /// A new logical recording was created.
    Imported {
        /// The new recording.
        recording_id: RecordingId,
        /// Source association created for it.
        source_item_id: SourceItemId,
        /// Asset the recording points at.
        media_asset_id: MediaAssetId,
        /// Whether an existing blob was reused rather than stored again.
        reused_existing_blob: bool,
    },
    /// The same source item was already imported; nothing changed.
    AlreadyImported {
        /// The recording that already represents this item.
        recording_id: RecordingId,
        /// The existing association.
        source_item_id: SourceItemId,
    },
    /// A media candidate was added to an existing recording.
    MediaCandidateAdded {
        /// The recording that gained a candidate.
        recording_id: RecordingId,
        /// The asset that was attached.
        media_asset_id: MediaAssetId,
        /// Whether an existing blob was reused.
        reused_existing_blob: bool,
    },
    /// The user deleted this item locally; a sync must not resurrect it.
    RefusedTombstoned {
        /// The tombstoned recording.
        recording_id: RecordingId,
    },
}

/// Imports media into the library.
#[derive(Debug, Clone)]
pub struct ImportService {
    journal: ImportJournal,
    assets: MediaAssetRepository,
    recordings: RecordingRepository,
    sources: SourceRepository,
}

impl ImportService {
    /// Build a service over an open database and archive.
    #[must_use]
    pub fn new(db: Database, archive: MediaArchive) -> Self {
        Self {
            journal: ImportJournal::new(db.clone(), archive),
            assets: MediaAssetRepository::new(db.clone()),
            recordings: RecordingRepository::new(db.clone()),
            sources: SourceRepository::new(db),
        }
    }

    /// The underlying journal, for startup reconciliation.
    #[must_use]
    pub fn journal(&self) -> &ImportJournal {
        &self.journal
    }

    /// Import one item.
    pub async fn import(
        &self,
        request: &ImportRequest,
        source: &mut dyn Read,
        at: Timestamp,
    ) -> PersistenceResult<ImportOutcome> {
        let source_id = self
            .sources
            .ensure_source(
                request.identity.source_kind,
                &request.identity.account_scope,
                request.identity.source_kind.provenance_label(),
                at,
            )
            .await?;

        // Record-level identity first: a stable external id makes the import
        // idempotent, and a local tombstone outranks any source state.
        if request.identity.is_stable() {
            if let Some(existing) = self
                .sources
                .find_by_identity(source_id, &request.identity)
                .await?
            {
                if self.recordings.is_tombstoned(existing.recording_id).await? {
                    return Ok(ImportOutcome::RefusedTombstoned {
                        recording_id: existing.recording_id,
                    });
                }
                self.sources
                    .update_remote_metadata(
                        existing.id,
                        request.remote_version.as_deref(),
                        RemoteState::Present,
                        at,
                    )
                    .await?;
                return Ok(ImportOutcome::AlreadyImported {
                    recording_id: existing.recording_id,
                    source_item_id: existing.id,
                });
            }
        }

        let staged = self.journal.stage(source, &request.observed_codec)?;
        let blob_existed = self
            .assets
            .find_by_identity(&staged.identity)
            .await?
            .is_some();

        let intent = self
            .journal
            .record_intent(
                &staged,
                &request.extension,
                request.quality_class,
                request.attach_to_recording,
                None,
                at,
            )
            .await?;
        self.journal.rename_into_archive(&intent, at).await?;
        let media_asset_id = self.journal.commit(&intent, at).await?;

        if let Some(recording_id) = request.attach_to_recording {
            self.add_candidate(request, recording_id, media_asset_id, blob_existed, at)
                .await
        } else {
            self.create_recording(request, source_id, media_asset_id, blob_existed, at)
                .await
        }
    }

    /// Attach a newly acquired asset to an existing recording as an additional
    /// media candidate. Re-attaching the same asset is a no-op, not an
    /// overwrite.
    async fn add_candidate(
        &self,
        request: &ImportRequest,
        recording_id: RecordingId,
        media_asset_id: MediaAssetId,
        reused_existing_blob: bool,
        at: Timestamp,
    ) -> PersistenceResult<ImportOutcome> {
        let existing = self.recordings.media_candidates(recording_id).await?;
        if !existing.iter().any(|c| c.media_asset_id == media_asset_id) {
            self.recordings
                .attach_media(
                    recording_id,
                    &RecordingMedia {
                        media_asset_id,
                        quality_class: request.quality_class,
                        obtained_at: at,
                    },
                )
                .await?;
        }
        Ok(ImportOutcome::MediaCandidateAdded {
            recording_id,
            media_asset_id,
            reused_existing_blob,
        })
    }

    /// Create a new logical meeting for a distinct source item.
    ///
    /// A distinct source item is always a distinct recording, even when the
    /// bytes are byte-identical to an existing one (spec 6.3B).
    async fn create_recording(
        &self,
        request: &ImportRequest,
        source_id: SourceId,
        media_asset_id: MediaAssetId,
        reused_existing_blob: bool,
        at: Timestamp,
    ) -> PersistenceResult<ImportOutcome> {
        let mut recording = Recording::new(RecordingId::new(), request.display_name.clone(), at);
        recording.tags = BTreeSet::new();
        self.recordings.insert(&recording).await?;
        self.recordings
            .attach_media(
                recording.id,
                &RecordingMedia {
                    media_asset_id,
                    quality_class: request.quality_class,
                    obtained_at: at,
                },
            )
            .await?;

        let association = SourceAssociation {
            id: SourceItemId::new(),
            source_id,
            identity: request.identity.clone(),
            recording_id: recording.id,
            import_status: ImportStatus::Imported,
            remote_state: RemoteState::Present,
            remote_version: request.remote_version.clone(),
            last_seen_at: Some(at),
            tombstone_reason: None,
        };
        self.sources.insert_item(&association, at).await?;

        Ok(ImportOutcome::Imported {
            recording_id: recording.id,
            source_item_id: association.id,
            media_asset_id,
            reused_existing_blob,
        })
    }

    /// Resolve the configured source for an identity key.
    pub async fn source_id(
        &self,
        identity: &SourceIdentityKey,
        at: Timestamp,
    ) -> PersistenceResult<SourceId> {
        self.sources
            .ensure_source(
                identity.source_kind,
                &identity.account_scope,
                identity.source_kind.provenance_label(),
                at,
            )
            .await
    }

    /// Recording repository, for callers that need to edit user-owned state.
    #[must_use]
    pub fn recordings(&self) -> &RecordingRepository {
        &self.recordings
    }

    /// Media asset repository.
    #[must_use]
    pub fn assets(&self) -> &MediaAssetRepository {
        &self.assets
    }

    /// Source repository.
    #[must_use]
    pub fn sources(&self) -> &SourceRepository {
        &self.sources
    }
}

/// Map an import failure onto the stable taxonomy.
#[must_use]
pub fn import_error_code(error: &PersistenceError) -> AppErrorCode {
    let code = error.code();
    debug_assert_ne!(code, AppErrorCode::InternalError);
    code
}

impl From<ImportOutcome> for Option<RecordingId> {
    fn from(outcome: ImportOutcome) -> Self {
        match outcome {
            ImportOutcome::Imported { recording_id, .. }
            | ImportOutcome::AlreadyImported { recording_id, .. }
            | ImportOutcome::MediaCandidateAdded { recording_id, .. }
            | ImportOutcome::RefusedTombstoned { recording_id } => Some(recording_id),
        }
    }
}

/// Convert a persistence failure into an application error.
#[must_use]
pub fn to_app_error(error: PersistenceError) -> AppError {
    AppError::from(error)
}
