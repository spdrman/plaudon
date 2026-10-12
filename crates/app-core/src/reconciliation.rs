//! The source reconciliation state machine (spec §7.5, §7.10, App. A.2).
//!
//! # The three facts that decide everything
//!
//! A source item carries a vendor **version hint**, a **content digest** we
//! computed ourselves, and a flag saying whether the acquisition that produced
//! that digest finished. §7.5 is explicit that the version is a hint: a new
//! media revision is created only if the bytes differ. Keeping the three apart
//! is what makes a changed version without changed bytes a cheap metadata
//! update, and an unchanged version with an unfinished acquisition a retry
//! rather than a permanent stall.
//!
//! # App. A.2, row by row
//!
//! | App. A.2 event | [`SourceEvent`] | Actions |
//! |---|---|---|
//! | New external ID | [`SourceEvent::NewExternalId`] | queue acquisition |
//! | Same external ID, unchanged version | [`SourceEvent::UnchangedVersion`] | skip |
//! | …with a prior acquisition that never finished | [`SourceEvent::UnchangedVersionIncompleteAcquisition`] | queue acquisition |
//! | Same external ID, changed version (metadata only) | [`SourceEvent::ChangedVersionMetadataOnly`] | reconcile metadata |
//! | Same external ID, changed version (audio changed) | [`SourceEvent::ChangedVersionContentChanged`] | reconcile metadata, append media revision |
//! | Same bytes from different source | [`SourceEvent::SameBytesFromDifferentSource`] | reuse blob, keep both associations |
//! | Remotely trashed/removed | [`SourceEvent::RemotelyTrashedOrRemoved`] | mark `remote_state` only |
//! | Local recording intentionally deleted | [`SourceEvent::LocallyDeleted`] | persist tombstone, refuse resurrection |
//! | Folder file still changing | [`SourceEvent::FolderFileStillChanging`] | defer and rescan |
//! | Folder cloud placeholder | [`SourceEvent::FolderCloudPlaceholder`] | wait for hydration, show status |
//! | Vendor-export transcript appears | [`SourceEvent::VendorExportTranscriptAppeared`] | import as a labelled vendor revision |
//! | User requests re-transcription | [`SourceEvent::UserRequestedRetranscription`] | create an independent new revision |
//!
//! The first six rows are decided by [`ReconciliationService::reconcile_listing`]
//! from an enumeration pass. The remaining rows are not things a listing can
//! tell us, so they have their own entry points:
//! [`ReconciliationService::record_local_deletion`],
//! [`classify_folder_candidate`] and
//! [`ReconciliationService::request_retranscription`]. A listing that carries
//! vendor artifacts produces an extra decision per artifact.
//!
//! # What "idempotent" means here
//!
//! A reconciliation pass reaches a fixpoint after one run: pass *n+1* and
//! pass *n+2* over an unchanged listing produce identical decision vectors and
//! write no additional rows. The first pass is allowed to differ from the
//! second, because its whole purpose is to move state — a changed version is
//! only changed once. What is never allowed is a second recording, a second
//! media revision, or a resurrected tombstone.
//!
//! # What this module never does
//!
//! It never deletes remote content, never deletes local media because a remote
//! item vanished, and never clears a tombstone except through
//! [`ReconciliationService::reimport`], which is reachable only from an
//! explicit user action and affects exactly one item.

use plaudon_domain::ids::{MediaAssetId, RecordingId, SourceId, SourceItemId};
use plaudon_domain::source::{RemoteState, SourceIdentityKey};
use plaudon_domain::time::Timestamp;
use plaudon_persistence::archive::MediaArchive;
use plaudon_persistence::db::Database;
use plaudon_persistence::error::{PersistenceError, PersistenceResult};
use plaudon_persistence::repo::{
    AcquisitionState, MediaAssetRepository, MediaRevisionRepository, RecordingRepository,
    SourceRepository,
};

use crate::vendor_artifacts::VendorArtifactRef;

/// One item as the source reported it during an enumeration pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedItem {
    /// Source-scoped identity triple (spec §7.2).
    pub identity: SourceIdentityKey,
    /// Name the source displays. Used for a new recording's initial title
    /// only; a local rename is never overwritten by it.
    pub display_name: String,
    /// Vendor version marker, when the connector exposes one. A hint.
    pub remote_version: Option<String>,
    /// Remote-side state the listing reported.
    pub remote_state: RemoteState,
    /// Duration the source reported, when it reports one.
    pub duration_ms: Option<u64>,
    /// Content digest, only when the bytes are already in hand.
    pub observed_content_sha256: Option<String>,
    /// Vendor-generated artifacts seen alongside the item.
    pub vendor_artifacts: Vec<VendorArtifactRef>,
}

impl ObservedItem {
    /// An item seen in a listing, with no bytes in hand yet.
    #[must_use]
    pub fn listed(identity: SourceIdentityKey, display_name: impl Into<String>) -> Self {
        Self {
            identity,
            display_name: display_name.into(),
            remote_version: None,
            remote_state: RemoteState::Present,
            duration_ms: None,
            observed_content_sha256: None,
            vendor_artifacts: Vec::new(),
        }
    }

    /// Set the vendor version hint.
    #[must_use]
    pub fn with_remote_version(mut self, version: impl Into<String>) -> Self {
        self.remote_version = Some(version.into());
        self
    }

    /// Set the remote-side state.
    #[must_use]
    pub fn with_remote_state(mut self, state: RemoteState) -> Self {
        self.remote_state = state;
        self
    }

    /// Record a digest we computed from bytes we actually hold.
    #[must_use]
    pub fn with_observed_content(mut self, sha256: impl Into<String>) -> Self {
        self.observed_content_sha256 = Some(sha256.into());
        self
    }

    /// Attach a vendor artifact observed alongside the item.
    #[must_use]
    pub fn with_vendor_artifact(mut self, artifact: VendorArtifactRef) -> Self {
        self.vendor_artifacts.push(artifact);
        self
    }
}

/// Why an acquisition is being queued.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcquisitionReason {
    /// The item has never been seen before.
    NewItem,
    /// The version is unchanged but the previous acquisition never finished.
    IncompletePriorAcquisition,
    /// The content digest moved, so new bytes must be fetched.
    ContentChanged,
    /// The user asked for the item again after deleting it.
    UserRequestedReimport,
}

/// One row of the App. A.2 event column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceEvent {
    /// An external identifier we have never associated with a recording.
    NewExternalId,
    /// Known item, unchanged version, prior acquisition complete.
    UnchangedVersion,
    /// Known item, unchanged version, prior acquisition incomplete.
    UnchangedVersionIncompleteAcquisition,
    /// Known item, changed version, identical or unknown bytes.
    ChangedVersionMetadataOnly,
    /// Known item, changed version, and the digest actually moved.
    ChangedVersionContentChanged,
    /// A new item whose bytes we already store under another association.
    SameBytesFromDifferentSource,
    /// The source reports the item as trashed, or stopped listing it.
    RemotelyTrashedOrRemoved,
    /// The user deleted the local recording. Outranks every source state.
    LocallyDeleted,
    /// A watched-folder candidate is still being written.
    FolderFileStillChanging,
    /// A watched-folder candidate is a cloud placeholder, not bytes.
    FolderCloudPlaceholder,
    /// A vendor-generated transcript appeared alongside the item.
    VendorExportTranscriptAppeared,
    /// The user asked for the recording to be transcribed again.
    UserRequestedRetranscription,
}

/// One row of the App. A.2 "Local action" column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReconcileAction {
    /// Acquire the bytes, after the caller's eligibility and consent
    /// preflight. Nothing is marked complete until bytes are committed.
    QueueAcquisition {
        /// Why the acquisition is needed.
        reason: AcquisitionReason,
    },
    /// Nothing to fetch.
    SkipAcquisition,
    /// Source-side metadata was updated. User-owned fields are untouched.
    ReconcileMetadata,
    /// A new immutable media revision was appended. The predecessor keeps
    /// pointing at the original bytes (spec §7.5).
    AppendMediaRevision {
        /// Asset the new revision points at.
        media_asset_id: MediaAssetId,
    },
    /// The bytes are already stored; the acquisition should reuse the blob.
    ReuseExistingMedia {
        /// Content-addressed asset to reuse.
        media_asset_id: MediaAssetId,
    },
    /// Both source associations survive; equal bytes never merge meetings.
    KeepSourceAssociation {
        /// The association that already exists, when there is one.
        existing: Option<SourceItemId>,
    },
    /// Only `source_items.remote_state` changed (spec §7.10).
    MarkRemoteState {
        /// The state that was recorded.
        remote_state: RemoteState,
    },
    /// A durable local tombstone was written.
    PersistTombstone {
        /// Why the user deleted it.
        reason: String,
    },
    /// A tombstoned recording must not be recreated by this pass.
    RefuseResurrection {
        /// The recording the user deleted.
        recording_id: RecordingId,
    },
    /// The candidate is not complete; rescan later. No conversion, no AI.
    DeferAndRescan {
        /// Suggested delay before the next probe.
        retry_after_ms: u64,
    },
    /// The candidate's bytes are not local; wait for OS-supported hydration.
    AwaitPlaceholderHydration {
        /// Suggested delay before the next probe.
        retry_after_ms: u64,
    },
    /// Hand the artifact to [`crate::vendor_artifacts::VendorArtifactImporter`],
    /// which applies the §7.5 gate and the human-edit protection.
    ImportVendorArtifact {
        /// The artifact that appeared.
        artifact: Box<VendorArtifactRef>,
    },
    /// Produce an independent new transcript revision. Existing outputs keep
    /// citing the revision they were generated from (App. A.2).
    CreateTranscriptRevision {
        /// Recording to transcribe again.
        recording_id: RecordingId,
    },
}

/// What the state machine decided for one item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconcileDecision {
    /// The item the decision is about.
    pub identity: SourceIdentityKey,
    /// Which App. A.2 row matched.
    pub event: SourceEvent,
    /// What was done, in order.
    pub actions: Vec<ReconcileAction>,
}

/// State of a watched-folder candidate that is not yet importable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FolderCandidateState {
    /// Size or modification time moved inside the quiescence window, or a
    /// readability or media check failed.
    StillChanging,
    /// The path is a cloud-drive placeholder; the bytes are not local.
    CloudPlaceholder,
}

/// Classify a watched-folder candidate that failed the completion barrier.
///
/// App. A.2 rows "Folder file still changing" and "Folder cloud placeholder".
/// Both defer; neither converts, dispatches AI or imports anything, because a
/// partial recording must never enter the pipeline.
#[must_use]
pub fn classify_folder_candidate(
    identity: &SourceIdentityKey,
    state: FolderCandidateState,
    retry_after_ms: u64,
) -> ReconcileDecision {
    let (event, action) = match state {
        FolderCandidateState::StillChanging => (
            SourceEvent::FolderFileStillChanging,
            ReconcileAction::DeferAndRescan { retry_after_ms },
        ),
        FolderCandidateState::CloudPlaceholder => (
            SourceEvent::FolderCloudPlaceholder,
            ReconcileAction::AwaitPlaceholderHydration { retry_after_ms },
        ),
    };
    ReconcileDecision {
        identity: identity.clone(),
        event,
        actions: vec![action],
    }
}

/// Classifies and persists one enumeration pass.
#[derive(Debug, Clone)]
pub struct ReconciliationService {
    sources: SourceRepository,
    recordings: RecordingRepository,
    assets: MediaAssetRepository,
    revisions: MediaRevisionRepository,
    archive: MediaArchive,
}

impl ReconciliationService {
    /// Build a service over an open database and media archive.
    #[must_use]
    pub fn new(db: Database, archive: MediaArchive) -> Self {
        Self {
            sources: SourceRepository::new(db.clone()),
            recordings: RecordingRepository::new(db.clone()),
            assets: MediaAssetRepository::new(db.clone()),
            revisions: MediaRevisionRepository::new(db),
            archive,
        }
    }

    /// The source repository, for callers that need the stored associations.
    #[must_use]
    pub fn sources(&self) -> &SourceRepository {
        &self.sources
    }

    /// The media revision repository, for callers reading revision history.
    #[must_use]
    pub fn media_revisions(&self) -> &MediaRevisionRepository {
        &self.revisions
    }

    /// Classify and persist one enumeration pass.
    ///
    /// Reaches a fixpoint after one run: re-running over the same listing
    /// produces the same decisions as the previous re-run and adds no rows.
    pub async fn reconcile_listing(
        &self,
        source_id: SourceId,
        observed: &[ObservedItem],
        at: Timestamp,
    ) -> PersistenceResult<Vec<ReconcileDecision>> {
        let mut decisions = Vec::with_capacity(observed.len());
        for item in observed {
            decisions.push(self.reconcile_item(source_id, item, at).await?);
            for artifact in &item.vendor_artifacts {
                decisions.push(ReconcileDecision {
                    identity: item.identity.clone(),
                    event: SourceEvent::VendorExportTranscriptAppeared,
                    actions: vec![ReconcileAction::ImportVendorArtifact {
                        artifact: Box::new(artifact.clone()),
                    }],
                });
            }
        }
        decisions.extend(self.reconcile_absent_items(source_id, observed, at).await?);
        Ok(decisions)
    }

    /// Items we hold that the listing no longer mentions (spec §7.5 "removed
    /// from listing"). They are marked absent and nothing else: the archived
    /// recording, its revisions and its derivatives all stay.
    async fn reconcile_absent_items(
        &self,
        source_id: SourceId,
        observed: &[ObservedItem],
        at: Timestamp,
    ) -> PersistenceResult<Vec<ReconcileDecision>> {
        let seen: Vec<&SourceIdentityKey> = observed.iter().map(|item| &item.identity).collect();
        let mut decisions = Vec::new();
        for stored in self.sources.list_items(source_id).await? {
            if seen.contains(&&stored.identity) {
                continue;
            }
            if self.is_tombstoned(&stored.id, stored.recording_id).await? {
                decisions.push(ReconcileDecision {
                    identity: stored.identity.clone(),
                    event: SourceEvent::LocallyDeleted,
                    actions: vec![ReconcileAction::RefuseResurrection {
                        recording_id: stored.recording_id,
                    }],
                });
                continue;
            }
            self.sources
                .set_remote_state(stored.id, RemoteState::Absent, at)
                .await?;
            decisions.push(ReconcileDecision {
                identity: stored.identity.clone(),
                event: SourceEvent::RemotelyTrashedOrRemoved,
                actions: vec![ReconcileAction::MarkRemoteState {
                    remote_state: RemoteState::Absent,
                }],
            });
        }
        Ok(decisions)
    }

    async fn is_tombstoned(
        &self,
        item: &SourceItemId,
        recording_id: RecordingId,
    ) -> PersistenceResult<bool> {
        Ok(self.sources.is_item_tombstoned(*item).await?
            || self.recordings.is_tombstoned(recording_id).await?)
    }

    async fn reconcile_item(
        &self,
        source_id: SourceId,
        item: &ObservedItem,
        at: Timestamp,
    ) -> PersistenceResult<ReconcileDecision> {
        let existing = self
            .sources
            .find_by_identity(source_id, &item.identity)
            .await?;

        let Some(association) = existing else {
            return self.classify_unknown_item(item).await;
        };

        // A local tombstone outranks everything the source says.
        if self
            .is_tombstoned(&association.id, association.recording_id)
            .await?
        {
            return Ok(ReconcileDecision {
                identity: item.identity.clone(),
                event: SourceEvent::LocallyDeleted,
                actions: vec![ReconcileAction::RefuseResurrection {
                    recording_id: association.recording_id,
                }],
            });
        }

        if matches!(item.remote_state, RemoteState::Trashed | RemoteState::Absent) {
            self.sources
                .set_remote_state(association.id, item.remote_state, at)
                .await?;
            return Ok(ReconcileDecision {
                identity: item.identity.clone(),
                event: SourceEvent::RemotelyTrashedOrRemoved,
                actions: vec![ReconcileAction::MarkRemoteState {
                    remote_state: item.remote_state,
                }],
            });
        }

        let acquisition = self.sources.acquisition_state(association.id).await?;
        if association.remote_version.as_deref() == item.remote_version.as_deref() {
            let (event, action) = if acquisition.complete {
                (SourceEvent::UnchangedVersion, ReconcileAction::SkipAcquisition)
            } else {
                (
                    SourceEvent::UnchangedVersionIncompleteAcquisition,
                    ReconcileAction::QueueAcquisition {
                        reason: AcquisitionReason::IncompletePriorAcquisition,
                    },
                )
            };
            return Ok(ReconcileDecision {
                identity: item.identity.clone(),
                event,
                actions: vec![action],
            });
        }

        self.reconcile_changed_version(item, &association, &acquisition, at)
            .await
    }

    /// An identity we hold no association for.
    async fn classify_unknown_item(
        &self,
        item: &ObservedItem,
    ) -> PersistenceResult<ReconcileDecision> {
        if let Some(sha) = item.observed_content_sha256.as_deref() {
            if let Some(asset) = self.assets.find_by_sha256(sha).await? {
                // Identical bytes under a different identity: reuse the blob,
                // keep both associations, never merge the meetings (§6.3B).
                return Ok(ReconcileDecision {
                    identity: item.identity.clone(),
                    event: SourceEvent::SameBytesFromDifferentSource,
                    actions: vec![
                        ReconcileAction::ReuseExistingMedia {
                            media_asset_id: asset.id,
                        },
                        ReconcileAction::KeepSourceAssociation { existing: None },
                    ],
                });
            }
        }
        Ok(ReconcileDecision {
            identity: item.identity.clone(),
            event: SourceEvent::NewExternalId,
            actions: vec![ReconcileAction::QueueAcquisition {
                reason: AcquisitionReason::NewItem,
            }],
        })
    }

    /// A known item whose version hint moved.
    ///
    /// The hint alone only reconciles metadata. A media revision is appended
    /// exactly when the digest we hold differs from the digest we observed and
    /// the new bytes are already committed to the archive; otherwise the item
    /// is marked incomplete so the next pass re-acquires it.
    async fn reconcile_changed_version(
        &self,
        item: &ObservedItem,
        association: &plaudon_domain::source::SourceAssociation,
        acquisition: &AcquisitionState,
        at: Timestamp,
    ) -> PersistenceResult<ReconcileDecision> {
        self.sources
            .update_remote_metadata(
                association.id,
                item.remote_version.as_deref(),
                RemoteState::Present,
                at,
            )
            .await?;
        if let Some(duration_ms) = item.duration_ms {
            self.recordings
                .set_duration_if_unset(association.recording_id, duration_ms)
                .await?;
        }

        let observed = item.observed_content_sha256.as_deref();
        let content_moved = match (observed, acquisition.observed_content_sha256.as_deref()) {
            (Some(new), Some(old)) => new != old,
            (Some(_), None) => true,
            (None, _) => false,
        };
        if !content_moved {
            return Ok(ReconcileDecision {
                identity: item.identity.clone(),
                event: SourceEvent::ChangedVersionMetadataOnly,
                actions: vec![ReconcileAction::ReconcileMetadata],
            });
        }

        let new_bytes = match observed {
            Some(sha) => self.assets.find_by_sha256(sha).await?,
            None => None,
        };
        let committed = match new_bytes.as_ref() {
            Some(asset) => self.archive.exists(&asset.relative_path)?,
            None => false,
        };
        let mut actions = vec![ReconcileAction::ReconcileMetadata];
        if let (Some(asset), true) = (new_bytes, committed) {
            self.revisions
                .append(
                    association.recording_id,
                    asset.id,
                    Some(association.id),
                    item.remote_version.as_deref(),
                    at,
                )
                .await?;
            self.sources
                .set_acquisition_state(
                    association.id,
                    &AcquisitionState {
                        observed_content_sha256: observed.map(ToOwned::to_owned),
                        complete: true,
                    },
                )
                .await?;
            actions.push(ReconcileAction::AppendMediaRevision {
                media_asset_id: asset.id,
            });
        } else {
            // The digest moved but the bytes are not here. Keep the digest we
            // can still verify, and re-acquire rather than guess.
            self.sources
                .set_acquisition_state(
                    association.id,
                    &AcquisitionState {
                        observed_content_sha256: acquisition.observed_content_sha256.clone(),
                        complete: false,
                    },
                )
                .await?;
            actions.push(ReconcileAction::QueueAcquisition {
                reason: AcquisitionReason::ContentChanged,
            });
        }
        Ok(ReconcileDecision {
            identity: item.identity.clone(),
            event: SourceEvent::ChangedVersionContentChanged,
            actions,
        })
    }

    /// Record that the user deleted a recording on purpose (App. A.2 row
    /// "Local recording intentionally deleted").
    ///
    /// Writes the durable tombstone that every later pass must respect. It
    /// does not delete media: which local assets are removed is the explicit
    /// deletion policy's decision, not reconciliation's.
    pub async fn record_local_deletion(
        &self,
        identity: &SourceIdentityKey,
        reason: &str,
        at: Timestamp,
    ) -> PersistenceResult<ReconcileDecision> {
        let association = self.require_association(identity).await?;
        self.recordings
            .tombstone(association.recording_id, reason, at, None)
            .await?;
        self.sources
            .tombstone_item(association.id, reason, at, None)
            .await?;
        Ok(ReconcileDecision {
            identity: identity.clone(),
            event: SourceEvent::LocallyDeleted,
            actions: vec![
                ReconcileAction::PersistTombstone {
                    reason: reason.to_string(),
                },
                ReconcileAction::RefuseResurrection {
                    recording_id: association.recording_id,
                },
            ],
        })
    }

    /// Classify an explicit user request to transcribe a recording again
    /// (App. A.2 row "User requests re-transcription").
    ///
    /// Writes nothing. The new revision is produced by the transcription job;
    /// existing AI outputs keep citing the revision they were generated from
    /// until the user regenerates them.
    pub async fn request_retranscription(
        &self,
        identity: &SourceIdentityKey,
    ) -> PersistenceResult<ReconcileDecision> {
        let association = self.require_association(identity).await?;
        Ok(ReconcileDecision {
            identity: identity.clone(),
            event: SourceEvent::UserRequestedRetranscription,
            actions: vec![ReconcileAction::CreateTranscriptRevision {
                recording_id: association.recording_id,
            }],
        })
    }

    /// Explicit user-initiated reimport of a tombstoned item (§7.10).
    ///
    /// Clears the refusal for exactly this item: its own tombstone, the
    /// tombstone on the recording it belongs to, and nothing else. The
    /// acquisition is marked incomplete so the next pass fetches bytes again.
    pub async fn reimport(&self, item: &SourceIdentityKey, at: Timestamp) -> PersistenceResult<()> {
        let association = self.require_association(item).await?;
        self.sources.clear_item_tombstone(association.id).await?;
        self.recordings
            .clear_tombstone(association.recording_id)
            .await?;
        let acquisition = self.sources.acquisition_state(association.id).await?;
        self.sources
            .set_acquisition_state(
                association.id,
                &AcquisitionState {
                    observed_content_sha256: acquisition.observed_content_sha256,
                    complete: false,
                },
            )
            .await?;
        self.sources
            .set_remote_state(association.id, RemoteState::Unknown, at)
            .await?;
        Ok(())
    }

    /// The action a reimport queues, for callers that want to report it.
    #[must_use]
    pub const fn reimport_action() -> ReconcileAction {
        ReconcileAction::QueueAcquisition {
            reason: AcquisitionReason::UserRequestedReimport,
        }
    }

    async fn require_association(
        &self,
        identity: &SourceIdentityKey,
    ) -> PersistenceResult<plaudon_domain::source::SourceAssociation> {
        let source_id = self
            .sources
            .find_source_id(identity.source_kind, &identity.account_scope)
            .await?
            .ok_or(PersistenceError::RecordNotFound)?;
        self.sources
            .find_by_identity(source_id, identity)
            .await?
            .ok_or(PersistenceError::RecordNotFound)
    }
}
