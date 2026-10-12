//! App. A.2 reconciliation table, §18.6A hostile cluster and the §7.5 vendor
//! artifact gate (tasks K2.1, K2.2, K2.5).
//!
//! Every row of App. A.2 has a test named after it. The table is a policy
//! specification, so the tests assert both halves of each row: the local
//! action taken, and the invariant the row exists to protect — above all that
//! no remote state ever deletes local content and no sync tick ever
//! resurrects a recording the user deleted.

use std::path::Path;

use plaudon_ai_contracts::stt::{TranscriptDraft, TranscriptSegmentDraft};
use plaudon_app_core::import::{ImportOutcome, ImportRequest, ImportService};
use plaudon_app_core::reconciliation::{
    classify_folder_candidate, AcquisitionReason, FolderCandidateState, ObservedItem,
    ReconcileAction, ReconcileDecision, ReconciliationService, SourceEvent,
};
use plaudon_app_core::vendor_artifacts::{
    VendorArtifactImporter, VendorArtifactKind, VendorArtifactRef, VendorExportPath,
    VendorImportCapability, VendorImportMode, VendorImportOutcome, VendorImportRefusal,
};
use plaudon_domain::ids::{RecordingId, SourceId, SourceItemId, TranscriptRevisionId};
use plaudon_domain::media::{QualityClass, Sha256Digest};
use plaudon_domain::model::{EngineId, ModelIdentity};
use plaudon_domain::source::{
    ExternalRecordingId, RemoteState, SourceAccountScope, SourceIdentityKey, SourceKind,
};
use plaudon_domain::time::Timestamp;
use plaudon_domain::transcript::{RevisionOrigin, RevisionStatus};
use plaudon_integration_tests::watcher::wav_bytes;
use plaudon_persistence::archive::MediaArchive;
use plaudon_persistence::migrations::MigrationRunner;
use plaudon_persistence::repo::{
    AcquisitionState, SourceRepository, TranscriptProvenanceRepository,
};
use plaudon_persistence::transcript::{RevisionContext, TranscriptRepository};
use plaudon_persistence::Database;

// ---------------------------------------------------------------- fixture --

struct Library {
    database: Database,
    archive: MediaArchive,
    imports: ImportService,
    reconcile: ReconciliationService,
}

impl Library {
    async fn open(base: &Path) -> Self {
        let database = Database::open(base.join("app.sqlite"))
            .await
            .expect("open database");
        MigrationRunner::new()
            .run(&database)
            .await
            .expect("migrate");
        let archive = MediaArchive::open(base.join("archive")).expect("archive");
        Self {
            imports: ImportService::new(database.clone(), archive.clone()),
            reconcile: ReconciliationService::new(database.clone(), archive.clone()),
            database,
            archive,
        }
    }

    /// Reopen the same library, modelling a restart.
    async fn reopen(base: &Path) -> Self {
        Self::open(base).await
    }

    fn sources(&self) -> SourceRepository {
        SourceRepository::new(self.database.clone())
    }

    async fn import(&self, identity: &SourceIdentityKey, bytes: &[u8], at: i64) -> ImportOutcome {
        let request = ImportRequest {
            identity: identity.clone(),
            display_name: "Fixture meeting".to_string(),
            quality_class: QualityClass::VendorCloudExport,
            extension: "wav".to_string(),
            observed_codec: "pcm".to_string(),
            remote_version: Some("v1".to_string()),
            attach_to_recording: None,
        };
        let mut reader = bytes;
        self.imports
            .import(&request, &mut reader, Timestamp::from_millis(at))
            .await
            .expect("import")
    }

    async fn source_id(&self, identity: &SourceIdentityKey) -> SourceId {
        self.imports
            .source_id(identity, Timestamp::from_millis(0))
            .await
            .expect("source id")
    }

    /// Row count for one of the tables these fixtures assert on.
    ///
    /// The table name is matched against literals rather than interpolated:
    /// `sqlx` refuses dynamic SQL, and so should a test.
    async fn count(&self, table: &str) -> i64 {
        let query = match table {
            "recordings" => sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM recordings"),
            "source_items" => sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM source_items"),
            "media_assets" => sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM media_assets"),
            "media_revisions" => {
                sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM media_revisions")
            }
            "transcript_revisions" => {
                sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM transcript_revisions")
            }
            "usage_events" => sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM usage_events"),
            "inference_attempts" => {
                sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM inference_attempts")
            }
            "jobs" => sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM jobs"),
            "source_export_artifacts" => {
                sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM source_export_artifacts")
            }
            other => panic!("unexpected table in a fixture: {other}"),
        };
        query.fetch_one(self.database.pool()).await.expect("count")
    }
}

fn cloud_identity(scope: &str, external: &str) -> SourceIdentityKey {
    SourceIdentityKey::new(
        SourceKind::PlaudCloudCli,
        SourceAccountScope::new(scope),
        Some(ExternalRecordingId::new(external)),
    )
}

fn observed(identity: &SourceIdentityKey, version: &str) -> ObservedItem {
    ObservedItem::listed(identity.clone(), "Fixture meeting").with_remote_version(version)
}

fn only(decisions: &[ReconcileDecision]) -> &ReconcileDecision {
    assert_eq!(decisions.len(), 1, "expected one decision: {decisions:?}");
    &decisions[0]
}

async fn item_id(library: &Library, identity: &SourceIdentityKey) -> SourceItemId {
    let source_id = library.source_id(identity).await;
    library
        .sources()
        .find_by_identity(source_id, identity)
        .await
        .expect("query")
        .expect("association")
        .id
}

// ------------------------------------------- App. A.2: new external ID -----

#[tokio::test]
async fn a2_new_external_id_queues_acquisition_and_commits_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let library = Library::open(dir.path()).await;
    let identity = cloud_identity("account-a", "rec-1");
    let source_id = library.source_id(&identity).await;

    let decisions = library
        .reconcile
        .reconcile_listing(
            source_id,
            &[observed(&identity, "v1")],
            Timestamp::from_millis(10),
        )
        .await
        .expect("reconcile");

    let decision = only(&decisions);
    assert_eq!(decision.event, SourceEvent::NewExternalId);
    assert_eq!(
        decision.actions,
        vec![ReconcileAction::QueueAcquisition {
            reason: AcquisitionReason::NewItem
        }]
    );
    // "Never mark complete without committed bytes": nothing was written.
    assert_eq!(library.count("recordings").await, 0);
    assert_eq!(library.count("source_items").await, 0);
    assert_eq!(library.count("media_assets").await, 0);
}

// ----------------------------- App. A.2: same external ID, unchanged -------

#[tokio::test]
async fn a2_unchanged_version_skips_the_download() {
    let dir = tempfile::tempdir().expect("tempdir");
    let library = Library::open(dir.path()).await;
    let identity = cloud_identity("account-a", "rec-1");
    library.import(&identity, &wav_bytes(1, 512), 10).await;
    let source_id = library.source_id(&identity).await;
    let item = item_id(&library, &identity).await;
    library
        .sources()
        .set_acquisition_state(
            item,
            &AcquisitionState {
                observed_content_sha256: Some(Sha256Digest::of(&wav_bytes(1, 512)).to_hex()),
                complete: true,
            },
        )
        .await
        .expect("mark complete");

    let decisions = library
        .reconcile
        .reconcile_listing(
            source_id,
            &[observed(&identity, "v1")],
            Timestamp::from_millis(20),
        )
        .await
        .expect("reconcile");
    let decision = only(&decisions);
    assert_eq!(decision.event, SourceEvent::UnchangedVersion);
    assert_eq!(decision.actions, vec![ReconcileAction::SkipAcquisition]);
    assert_eq!(library.count("media_revisions").await, 0);
}

#[tokio::test]
async fn a2_unchanged_version_with_an_incomplete_acquisition_re_downloads() {
    let dir = tempfile::tempdir().expect("tempdir");
    let library = Library::open(dir.path()).await;
    let identity = cloud_identity("account-a", "rec-1");
    library.import(&identity, &wav_bytes(1, 512), 10).await;
    let source_id = library.source_id(&identity).await;
    // The default is `acquisition_complete = 0`, which is exactly the state a
    // crashed download leaves behind.

    let decisions = library
        .reconcile
        .reconcile_listing(
            source_id,
            &[observed(&identity, "v1")],
            Timestamp::from_millis(20),
        )
        .await
        .expect("reconcile");
    let decision = only(&decisions);
    assert_eq!(
        decision.event,
        SourceEvent::UnchangedVersionIncompleteAcquisition
    );
    assert_eq!(
        decision.actions,
        vec![ReconcileAction::QueueAcquisition {
            reason: AcquisitionReason::IncompletePriorAcquisition
        }]
    );
}

// ------------------------------- App. A.2: same external ID, changed -------

#[tokio::test]
async fn a2_changed_version_without_changed_bytes_reconciles_metadata_only() {
    let dir = tempfile::tempdir().expect("tempdir");
    let library = Library::open(dir.path()).await;
    let identity = cloud_identity("account-a", "rec-1");
    library.import(&identity, &wav_bytes(1, 512), 10).await;
    let source_id = library.source_id(&identity).await;
    let item = item_id(&library, &identity).await;
    let digest = Sha256Digest::of(&wav_bytes(1, 512)).to_hex();
    library
        .sources()
        .set_acquisition_state(
            item,
            &AcquisitionState {
                observed_content_sha256: Some(digest.clone()),
                complete: true,
            },
        )
        .await
        .expect("mark complete");

    let decisions = library
        .reconcile
        .reconcile_listing(
            source_id,
            &[observed(&identity, "v2").with_observed_content(digest)],
            Timestamp::from_millis(20),
        )
        .await
        .expect("reconcile");
    let decision = only(&decisions);
    assert_eq!(decision.event, SourceEvent::ChangedVersionMetadataOnly);
    assert_eq!(decision.actions, vec![ReconcileAction::ReconcileMetadata]);
    assert_eq!(
        library.count("media_revisions").await,
        0,
        "a version hint alone never creates a revision"
    );

    let refreshed = library
        .sources()
        .get_item(item)
        .await
        .expect("reload association");
    assert_eq!(refreshed.remote_version.as_deref(), Some("v2"));
}

#[tokio::test]
async fn a2_changed_version_with_changed_bytes_appends_a_media_revision() {
    let dir = tempfile::tempdir().expect("tempdir");
    let library = Library::open(dir.path()).await;
    let identity = cloud_identity("account-a", "rec-1");
    let original = wav_bytes(1, 512);
    let replacement = wav_bytes(2, 900);
    let ImportOutcome::Imported {
        recording_id,
        media_asset_id: original_asset,
        ..
    } = library.import(&identity, &original, 10).await
    else {
        panic!("expected an import");
    };
    let item = item_id(&library, &identity).await;
    library
        .sources()
        .set_acquisition_state(
            item,
            &AcquisitionState {
                observed_content_sha256: Some(Sha256Digest::of(&original).to_hex()),
                complete: true,
            },
        )
        .await
        .expect("mark complete");

    // The new bytes have been acquired under their own association, so the
    // blob exists and the revision can point at it.
    let other = cloud_identity("account-b", "rec-9");
    library.import(&other, &replacement, 20).await;

    let source_id = library.source_id(&identity).await;
    let decisions = library
        .reconcile
        .reconcile_listing(
            source_id,
            &[observed(&identity, "v2")
                .with_observed_content(Sha256Digest::of(&replacement).to_hex())],
            Timestamp::from_millis(30),
        )
        .await
        .expect("reconcile");
    let decision = only(&decisions);
    assert_eq!(decision.event, SourceEvent::ChangedVersionContentChanged);
    assert!(decision
        .actions
        .contains(&ReconcileAction::ReconcileMetadata));
    let appended = decision
        .actions
        .iter()
        .any(|action| matches!(action, ReconcileAction::AppendMediaRevision { .. }));
    assert!(appended, "{:?}", decision.actions);

    let revisions = library
        .reconcile
        .media_revisions()
        .list(recording_id)
        .await
        .expect("revisions");
    assert_eq!(revisions.len(), 1);
    assert_ne!(revisions[0].media_asset_id, original_asset);

    // The original audio is untouched and still the preferred candidate.
    let recording = library
        .imports
        .recordings()
        .get(recording_id)
        .await
        .expect("recording");
    assert_eq!(recording.preferred_media_asset_id(), Some(original_asset));
    assert!(library.imports.assets().get(original_asset).await.is_ok());
}

// ------------------------- App. A.2: same bytes from a different source ----

#[tokio::test]
async fn a2_same_bytes_from_a_different_source_reuse_one_blob_and_keep_both_associations() {
    let dir = tempfile::tempdir().expect("tempdir");
    let library = Library::open(dir.path()).await;
    let bytes = wav_bytes(5, 2_048);
    let first = cloud_identity("account-a", "rec-1");
    let second = cloud_identity("account-b", "rec-1");

    library.import(&first, &bytes, 10).await;

    // Before acquiring the second item, the listing already recognises the
    // digest as one we store.
    let second_source = library.source_id(&second).await;
    let decisions = library
        .reconcile
        .reconcile_listing(
            second_source,
            &[observed(&second, "v1").with_observed_content(Sha256Digest::of(&bytes).to_hex())],
            Timestamp::from_millis(20),
        )
        .await
        .expect("reconcile");
    let decision = only(&decisions);
    assert_eq!(decision.event, SourceEvent::SameBytesFromDifferentSource);
    assert!(decision
        .actions
        .iter()
        .any(|action| matches!(action, ReconcileAction::ReuseExistingMedia { .. })));

    let outcome = library.import(&second, &bytes, 30).await;
    match outcome {
        ImportOutcome::Imported {
            reused_existing_blob,
            ..
        } => assert!(reused_existing_blob, "the blob must be reused"),
        other => panic!("expected an import, got {other:?}"),
    }

    assert_eq!(library.count("media_assets").await, 1, "one blob");
    assert_eq!(library.count("source_items").await, 2, "both associations");
    assert_eq!(
        library.count("recordings").await,
        2,
        "two accounts are two logical meetings (§6.3B)"
    );
    assert_eq!(library.archive.list_blobs().expect("blobs").len(), 1);
}

// --------------------------- App. A.2: remotely trashed or removed ---------

#[tokio::test]
async fn a2_remotely_trashed_marks_remote_state_and_retains_everything() {
    let dir = tempfile::tempdir().expect("tempdir");
    let library = Library::open(dir.path()).await;
    let identity = cloud_identity("account-a", "rec-1");
    library.import(&identity, &wav_bytes(1, 512), 10).await;
    let source_id = library.source_id(&identity).await;
    let blobs_before = library.archive.list_blobs().expect("blobs").len();

    let decisions = library
        .reconcile
        .reconcile_listing(
            source_id,
            &[observed(&identity, "v1").with_remote_state(RemoteState::Trashed)],
            Timestamp::from_millis(20),
        )
        .await
        .expect("reconcile");
    let decision = only(&decisions);
    assert_eq!(decision.event, SourceEvent::RemotelyTrashedOrRemoved);
    assert_eq!(
        decision.actions,
        vec![ReconcileAction::MarkRemoteState {
            remote_state: RemoteState::Trashed
        }]
    );

    let item = item_id(&library, &identity).await;
    let association = library.sources().get_item(item).await.expect("reload");
    assert_eq!(association.remote_state, RemoteState::Trashed);
    assert!(!association.remote_state.grants_local_deletion());
    assert_eq!(library.count("recordings").await, 1);
    assert_eq!(library.count("media_assets").await, 1);
    assert_eq!(
        library.archive.list_blobs().expect("blobs").len(),
        blobs_before
    );
}

#[tokio::test]
async fn a2_an_item_missing_from_the_listing_is_marked_absent_not_deleted() {
    let dir = tempfile::tempdir().expect("tempdir");
    let library = Library::open(dir.path()).await;
    let identity = cloud_identity("account-a", "rec-1");
    library.import(&identity, &wav_bytes(1, 512), 10).await;
    let source_id = library.source_id(&identity).await;

    let decisions = library
        .reconcile
        .reconcile_listing(source_id, &[], Timestamp::from_millis(20))
        .await
        .expect("reconcile");
    let decision = only(&decisions);
    assert_eq!(decision.event, SourceEvent::RemotelyTrashedOrRemoved);
    assert_eq!(
        decision.actions,
        vec![ReconcileAction::MarkRemoteState {
            remote_state: RemoteState::Absent
        }]
    );
    assert_eq!(library.count("recordings").await, 1);
    assert_eq!(library.count("media_assets").await, 1);
}

/// §18.6A: trash, then restore. Neither direction touches local content.
#[tokio::test]
async fn a_remote_trash_followed_by_a_restore_round_trips_without_data_loss() {
    let dir = tempfile::tempdir().expect("tempdir");
    let library = Library::open(dir.path()).await;
    let identity = cloud_identity("account-a", "rec-1");
    library.import(&identity, &wav_bytes(1, 512), 10).await;
    let source_id = library.source_id(&identity).await;
    let item = item_id(&library, &identity).await;
    library
        .sources()
        .set_acquisition_state(
            item,
            &AcquisitionState {
                observed_content_sha256: Some(Sha256Digest::of(&wav_bytes(1, 512)).to_hex()),
                complete: true,
            },
        )
        .await
        .expect("complete");

    library
        .reconcile
        .reconcile_listing(
            source_id,
            &[observed(&identity, "v1").with_remote_state(RemoteState::Trashed)],
            Timestamp::from_millis(20),
        )
        .await
        .expect("trash");
    assert_eq!(
        library
            .sources()
            .get_item(item)
            .await
            .expect("row")
            .remote_state,
        RemoteState::Trashed
    );

    let restored = library
        .reconcile
        .reconcile_listing(
            source_id,
            &[observed(&identity, "v1")],
            Timestamp::from_millis(30),
        )
        .await
        .expect("restore");
    assert_eq!(only(&restored).event, SourceEvent::UnchangedVersion);
    assert_eq!(library.count("recordings").await, 1);
    assert_eq!(library.count("media_assets").await, 1);
}

// --------------------- App. A.2: local recording intentionally deleted -----

#[tokio::test]
async fn a2_a_local_tombstone_outranks_every_source_state() {
    let dir = tempfile::tempdir().expect("tempdir");
    let library = Library::open(dir.path()).await;
    let identity = cloud_identity("account-a", "rec-1");
    library.import(&identity, &wav_bytes(1, 512), 10).await;
    let source_id = library.source_id(&identity).await;

    let deletion = library
        .reconcile
        .record_local_deletion(
            &identity,
            "user deleted the meeting",
            Timestamp::from_millis(20),
        )
        .await
        .expect("tombstone");
    assert_eq!(deletion.event, SourceEvent::LocallyDeleted);
    assert!(deletion
        .actions
        .iter()
        .any(|action| matches!(action, ReconcileAction::PersistTombstone { .. })));

    // The source still lists it, in perfect health. It stays deleted.
    let decisions = library
        .reconcile
        .reconcile_listing(
            source_id,
            &[observed(&identity, "v2")
                .with_observed_content(Sha256Digest::of(&wav_bytes(9, 900)).to_hex())],
            Timestamp::from_millis(30),
        )
        .await
        .expect("reconcile");
    let decision = only(&decisions);
    assert_eq!(decision.event, SourceEvent::LocallyDeleted);
    assert!(matches!(
        decision.actions.as_slice(),
        [ReconcileAction::RefuseResurrection { .. }]
    ));
    assert_eq!(library.count("media_revisions").await, 0);
}

/// §18.6A: the tombstone survives a restart and still blocks the sync tick.
#[tokio::test]
async fn a_tombstone_survives_a_restart_and_still_blocks_the_next_sync_tick() {
    let dir = tempfile::tempdir().expect("tempdir");
    let identity = cloud_identity("account-a", "rec-1");
    {
        let library = Library::open(dir.path()).await;
        library.import(&identity, &wav_bytes(1, 512), 10).await;
        library
            .reconcile
            .record_local_deletion(&identity, "user deleted it", Timestamp::from_millis(20))
            .await
            .expect("tombstone");
        library.database.close().await;
    }

    let library = Library::reopen(dir.path()).await;
    let source_id = library.source_id(&identity).await;
    let decisions = library
        .reconcile
        .reconcile_listing(
            source_id,
            &[observed(&identity, "v1")],
            Timestamp::from_millis(30),
        )
        .await
        .expect("reconcile");
    assert_eq!(only(&decisions).event, SourceEvent::LocallyDeleted);

    // And a direct import attempt is refused too.
    let outcome = library.import(&identity, &wav_bytes(1, 512), 40).await;
    assert!(matches!(outcome, ImportOutcome::RefusedTombstoned { .. }));
    assert_eq!(library.count("recordings").await, 1);
}

#[tokio::test]
async fn reimport_lifts_the_refusal_for_one_item_and_only_that_item() {
    let dir = tempfile::tempdir().expect("tempdir");
    let library = Library::open(dir.path()).await;
    let kept = cloud_identity("account-a", "rec-1");
    let other = cloud_identity("account-a", "rec-2");
    library.import(&kept, &wav_bytes(1, 512), 10).await;
    library.import(&other, &wav_bytes(2, 512), 11).await;
    library
        .reconcile
        .record_local_deletion(&kept, "mistake", Timestamp::from_millis(20))
        .await
        .expect("tombstone kept");
    library
        .reconcile
        .record_local_deletion(&other, "deliberate", Timestamp::from_millis(21))
        .await
        .expect("tombstone other");

    library
        .reconcile
        .reimport(&kept, Timestamp::from_millis(30))
        .await
        .expect("reimport");

    let source_id = library.source_id(&kept).await;
    let decisions = library
        .reconcile
        .reconcile_listing(
            source_id,
            &[observed(&kept, "v1"), observed(&other, "v1")],
            Timestamp::from_millis(40),
        )
        .await
        .expect("reconcile");
    let for_kept = decisions
        .iter()
        .find(|d| d.identity == kept)
        .expect("decision for the reimported item");
    assert_eq!(
        for_kept.event,
        SourceEvent::UnchangedVersionIncompleteAcquisition,
        "a reimport re-acquires rather than assuming the old bytes"
    );
    let for_other = decisions
        .iter()
        .find(|d| d.identity == other)
        .expect("decision for the still-deleted item");
    assert_eq!(for_other.event, SourceEvent::LocallyDeleted);
}

// ------------------------------- App. A.2: watched-folder rows -------------

#[test]
fn a2_a_folder_file_still_changing_defers_without_any_dispatch() {
    let identity = SourceIdentityKey::new(
        SourceKind::WatchedFolder,
        SourceAccountScope::new("/tmp/watched"),
        None,
    );
    let decision = classify_folder_candidate(&identity, FolderCandidateState::StillChanging, 2_000);
    assert_eq!(decision.event, SourceEvent::FolderFileStillChanging);
    assert_eq!(
        decision.actions,
        vec![ReconcileAction::DeferAndRescan {
            retry_after_ms: 2_000
        }]
    );
}

#[test]
fn a2_a_folder_cloud_placeholder_waits_for_hydration_and_shows_status() {
    let identity = SourceIdentityKey::new(
        SourceKind::WatchedFolder,
        SourceAccountScope::new("/tmp/watched"),
        None,
    );
    let decision =
        classify_folder_candidate(&identity, FolderCandidateState::CloudPlaceholder, 5_000);
    assert_eq!(decision.event, SourceEvent::FolderCloudPlaceholder);
    assert_eq!(
        decision.actions,
        vec![ReconcileAction::AwaitPlaceholderHydration {
            retry_after_ms: 5_000
        }]
    );
}

// --------------------------- App. A.2: vendor-export transcript appears ----

fn vendor_transcript(body: &str) -> VendorArtifactRef {
    VendorArtifactRef::transcript(
        "plaud",
        body.as_bytes(),
        VendorExportPath::OfficialCli {
            release: "plaud-cli 1.4.0".to_string(),
        },
    )
    .with_original_filename("meeting-transcript.txt")
}

#[tokio::test]
async fn a2_a_vendor_export_transcript_surfaces_as_its_own_decision() {
    let dir = tempfile::tempdir().expect("tempdir");
    let library = Library::open(dir.path()).await;
    let identity = cloud_identity("account-a", "rec-1");
    library.import(&identity, &wav_bytes(1, 512), 10).await;
    let source_id = library.source_id(&identity).await;

    let item = observed(&identity, "v1").with_vendor_artifact(vendor_transcript("hello\nworld"));
    let decisions = library
        .reconcile
        .reconcile_listing(source_id, &[item], Timestamp::from_millis(20))
        .await
        .expect("reconcile");
    assert_eq!(decisions.len(), 2, "{decisions:?}");
    assert_eq!(
        decisions[1].event,
        SourceEvent::VendorExportTranscriptAppeared
    );
    assert!(matches!(
        decisions[1].actions.as_slice(),
        [ReconcileAction::ImportVendorArtifact { .. }]
    ));
}

#[tokio::test]
async fn the_production_default_refuses_vendor_imports_with_a_stated_reason() {
    let dir = tempfile::tempdir().expect("tempdir");
    let library = Library::open(dir.path()).await;
    let identity = cloud_identity("account-a", "rec-1");
    let ImportOutcome::Imported { recording_id, .. } =
        library.import(&identity, &wav_bytes(1, 512), 10).await
    else {
        panic!("expected an import");
    };
    let item = item_id(&library, &identity).await;

    let importer = VendorArtifactImporter::new(library.database.clone());
    let body = "vendor produced this";
    let request = plaudon_app_core::vendor_artifacts::VendorImportRequest {
        recording_id,
        source_item_id: item,
        artifact: vendor_transcript(body),
        mode: VendorImportMode::RetainBoth,
        capability: VendorImportCapability::production_default(),
    };
    assert!(!request.capability.is_enabled());

    let outcome = importer
        .import_transcript(&request, body, Timestamp::from_millis(20))
        .await
        .expect("import attempt");
    match outcome {
        VendorImportOutcome::Refused { refusal, .. } => {
            assert_eq!(refusal, VendorImportRefusal::CapabilityNotVerified);
            assert!(refusal.reason().contains("official export path"));
        }
        imported @ VendorImportOutcome::Imported { .. } => {
            panic!("the default build must refuse: {imported:?}")
        }
    }
    assert_eq!(library.count("transcript_revisions").await, 0);
    // The observation is still recorded, so the UI can explain itself.
    let artifacts = importer.artifacts_for(item).await.expect("artifacts");
    assert_eq!(artifacts.len(), 1);
    assert!(artifacts[0].imported_at.is_none());
}

#[tokio::test]
async fn an_unofficial_export_path_is_refused_even_when_the_feature_is_enabled() {
    let dir = tempfile::tempdir().expect("tempdir");
    let library = Library::open(dir.path()).await;
    let identity = cloud_identity("account-a", "rec-1");
    let ImportOutcome::Imported { recording_id, .. } =
        library.import(&identity, &wav_bytes(1, 512), 10).await
    else {
        panic!("expected an import");
    };
    let item = item_id(&library, &identity).await;
    let importer = VendorArtifactImporter::new(library.database.clone());
    let body = "scraped from an undocumented endpoint";

    let artifact = VendorArtifactRef::transcript(
        "plaud",
        body.as_bytes(),
        VendorExportPath::Unofficial {
            description: "undocumented web API".to_string(),
        },
    );
    let request = plaudon_app_core::vendor_artifacts::VendorImportRequest {
        recording_id,
        source_item_id: item,
        artifact,
        mode: VendorImportMode::ImportVendorOnly,
        capability: VendorImportCapability::verified_official_export("fixture evidence"),
    };
    let outcome = importer
        .import_transcript(&request, body, Timestamp::from_millis(20))
        .await
        .expect("import attempt");
    assert!(matches!(
        outcome,
        VendorImportOutcome::Refused {
            refusal: VendorImportRefusal::UnofficialExportPath,
            ..
        }
    ));
    assert_eq!(library.count("transcript_revisions").await, 0);
}

#[tokio::test]
async fn skip_vendor_content_is_the_default_mode_and_imports_audio_only() {
    assert_eq!(
        VendorImportMode::default(),
        VendorImportMode::SkipVendorContent
    );

    let dir = tempfile::tempdir().expect("tempdir");
    let library = Library::open(dir.path()).await;
    let identity = cloud_identity("account-a", "rec-1");
    let ImportOutcome::Imported { recording_id, .. } =
        library.import(&identity, &wav_bytes(1, 512), 10).await
    else {
        panic!("expected an import");
    };
    let item = item_id(&library, &identity).await;
    let importer = VendorArtifactImporter::new(library.database.clone());
    let body = "vendor transcript";
    let request = plaudon_app_core::vendor_artifacts::VendorImportRequest {
        recording_id,
        source_item_id: item,
        artifact: vendor_transcript(body),
        mode: VendorImportMode::default(),
        capability: VendorImportCapability::verified_official_export("fixture evidence"),
    };
    let outcome = importer
        .import_transcript(&request, body, Timestamp::from_millis(20))
        .await
        .expect("import attempt");
    assert!(matches!(
        outcome,
        VendorImportOutcome::Refused {
            refusal: VendorImportRefusal::ModeSkipsVendorContent,
            ..
        }
    ));
    assert_eq!(library.count("transcript_revisions").await, 0);
    assert_eq!(library.count("media_assets").await, 1, "the audio is kept");
}

#[tokio::test]
async fn import_vendor_only_creates_a_labelled_revision_and_charges_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let library = Library::open(dir.path()).await;
    let identity = cloud_identity("account-a", "rec-1");
    let ImportOutcome::Imported { recording_id, .. } =
        library.import(&identity, &wav_bytes(1, 512), 10).await
    else {
        panic!("expected an import");
    };
    let item = item_id(&library, &identity).await;
    let importer = VendorArtifactImporter::new(library.database.clone());
    let body = "line one\nline two";
    let request = plaudon_app_core::vendor_artifacts::VendorImportRequest {
        recording_id,
        source_item_id: item,
        artifact: vendor_transcript(body),
        mode: VendorImportMode::ImportVendorOnly,
        capability: VendorImportCapability::verified_official_export("plaud-cli 1.4.0 fixture"),
    };

    let outcome = importer
        .import_transcript(&request, body, Timestamp::from_millis(20))
        .await
        .expect("import");
    let VendorImportOutcome::Imported { revision_id, .. } = outcome else {
        panic!("expected an import: {outcome:?}");
    };

    let provenance = TranscriptProvenanceRepository::new(library.database.clone());
    assert_eq!(
        provenance.source_vendor(revision_id).await.expect("vendor"),
        Some("plaud".to_string())
    );
    let rows = provenance
        .list_for_recording(recording_id)
        .await
        .expect("rows");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].origin, "machine");
    assert!(rows[0].is_vendor());

    // §7.5: no AI credits. Nothing was dispatched, so nothing is billed.
    assert_eq!(library.count("usage_events").await, 0);
    assert_eq!(library.count("inference_attempts").await, 0);
    assert_eq!(library.count("jobs").await, 0);

    // The original bytes stay attributable.
    let artifacts = importer.artifacts_for(item).await.expect("artifacts");
    assert_eq!(artifacts.len(), 1);
    assert_eq!(
        artifacts[0].artifact_kind,
        VendorArtifactKind::Transcript.as_str()
    );
    assert_eq!(
        artifacts[0].content_sha256,
        Sha256Digest::of(body.as_bytes()).to_hex()
    );
    assert_eq!(artifacts[0].byte_length, body.len() as u64);
    assert_eq!(
        artifacts[0].original_filename.as_deref(),
        Some("meeting-transcript.txt")
    );
    assert!(artifacts[0].imported_at.is_some());

    // The text itself is preserved verbatim, line for line.
    let stored = TranscriptRepository::new(library.database.clone())
        .segments(revision_id)
        .await
        .expect("segments");
    let joined: Vec<String> = stored.into_iter().map(|segment| segment.text).collect();
    assert_eq!(joined, vec!["line one".to_string(), "line two".to_string()]);
}

/// §18.6A: a vendor export must never silently replace a human correction.
#[tokio::test]
async fn a_vendor_export_never_replaces_or_demotes_a_human_corrected_transcript() {
    let dir = tempfile::tempdir().expect("tempdir");
    let library = Library::open(dir.path()).await;
    let identity = cloud_identity("account-a", "rec-1");
    let ImportOutcome::Imported { recording_id, .. } =
        library.import(&identity, &wav_bytes(1, 512), 10).await
    else {
        panic!("expected an import");
    };
    let item = item_id(&library, &identity).await;
    let human = insert_human_edit(&library, recording_id, "the user fixed this line").await;

    let importer = VendorArtifactImporter::new(library.database.clone());
    let body = "the vendor thinks it said something else";
    let enabled = VendorImportCapability::verified_official_export("plaud-cli 1.4.0 fixture");

    // `import_vendor_only` would displace the correction, so it is refused.
    let displacing = plaudon_app_core::vendor_artifacts::VendorImportRequest {
        recording_id,
        source_item_id: item,
        artifact: vendor_transcript(body),
        mode: VendorImportMode::ImportVendorOnly,
        capability: enabled.clone(),
    };
    let refused = importer
        .import_transcript(&displacing, body, Timestamp::from_millis(20))
        .await
        .expect("attempt");
    assert!(matches!(
        refused,
        VendorImportOutcome::Refused {
            refusal: VendorImportRefusal::HumanCorrectedTranscriptExists,
            ..
        }
    ));

    // `retain_both` keeps them side by side, human edit still preferred.
    let retaining = plaudon_app_core::vendor_artifacts::VendorImportRequest {
        mode: VendorImportMode::RetainBoth,
        ..displacing
    };
    let outcome = importer
        .import_transcript(&retaining, body, Timestamp::from_millis(30))
        .await
        .expect("attempt");
    let VendorImportOutcome::Imported {
        revision_id,
        human_edit_retained,
        ..
    } = outcome
    else {
        panic!("retain_both must import: {outcome:?}");
    };
    assert!(human_edit_retained);
    assert_ne!(revision_id, human);

    let provenance = TranscriptProvenanceRepository::new(library.database.clone());
    let preferred = provenance
        .preferred(recording_id)
        .await
        .expect("preferred")
        .expect("a revision");
    assert_eq!(preferred.id, human, "the human edit stays preferred");
    assert!(preferred.is_human_edit());
    assert!(!preferred.is_vendor());

    // The human revision's own text is byte-for-byte what the user wrote.
    let stored = TranscriptRepository::new(library.database.clone())
        .segments(human)
        .await
        .expect("segments");
    assert_eq!(stored[0].text, "the user fixed this line");
}

// ------------------------- App. A.2: user requests re-transcription --------

#[tokio::test]
async fn a2_re_transcription_creates_a_new_revision_and_old_outputs_keep_their_input() {
    let dir = tempfile::tempdir().expect("tempdir");
    let library = Library::open(dir.path()).await;
    let identity = cloud_identity("account-a", "rec-1");
    let ImportOutcome::Imported { recording_id, .. } =
        library.import(&identity, &wav_bytes(1, 512), 10).await
    else {
        panic!("expected an import");
    };
    let first = insert_machine_revision(&library, recording_id, "first pass").await;
    insert_ai_output(&library, recording_id, first).await;

    let decision = library
        .reconcile
        .request_retranscription(&identity)
        .await
        .expect("classify");
    assert_eq!(decision.event, SourceEvent::UserRequestedRetranscription);
    assert_eq!(
        decision.actions,
        vec![ReconcileAction::CreateTranscriptRevision { recording_id }]
    );

    // The job then produces an independent revision.
    let second = insert_machine_revision(&library, recording_id, "second pass").await;
    assert_ne!(first, second);

    let cited: String = sqlx::query_scalar("SELECT transcript_revision_id FROM ai_outputs LIMIT 1")
        .fetch_one(library.database.pool())
        .await
        .expect("output");
    assert_eq!(
        cited,
        first.to_string(),
        "an existing summary keeps citing the revision it was generated from"
    );
    assert_eq!(library.count("transcript_revisions").await, 2);
}

// ------------------------------------------------------------- helpers -----

async fn insert_human_edit(
    library: &Library,
    recording_id: RecordingId,
    text: &str,
) -> TranscriptRevisionId {
    insert_revision(library, recording_id, text, RevisionOrigin::HumanEdit).await
}

async fn insert_machine_revision(
    library: &Library,
    recording_id: RecordingId,
    text: &str,
) -> TranscriptRevisionId {
    insert_revision(library, recording_id, text, RevisionOrigin::Machine).await
}

async fn insert_revision(
    library: &Library,
    recording_id: RecordingId,
    text: &str,
    origin: RevisionOrigin,
) -> TranscriptRevisionId {
    let engine = EngineId::new("whisper-local");
    let draft = TranscriptDraft::text_only(
        engine.clone(),
        ModelIdentity::new(engine, "base.en"),
        Sha256Digest::of(text.as_bytes()),
        vec![TranscriptSegmentDraft::text_only(0, text)],
    );
    let context = RevisionContext {
        recording_id,
        parent_id: None,
        origin,
        status: RevisionStatus::Complete,
        created_at: Timestamp::from_millis(100),
        provenance: None,
    };
    TranscriptRepository::new(library.database.clone())
        .save_draft(&draft, &context)
        .await
        .expect("save revision")
}

async fn insert_ai_output(
    library: &Library,
    recording_id: RecordingId,
    revision_id: TranscriptRevisionId,
) {
    sqlx::query(
        "INSERT INTO ai_outputs
             (id, recording_id, output_type, transcript_revision_id, template_version,
              model_id, prompt_version, schema_version, body_json, status, created_at)
         VALUES (?1, ?2, 'summary', ?3, 'v1', 'local', 'v1', 'v1', '{}', 'complete', 0)",
    )
    .bind(uuid_like())
    .bind(recording_id.to_string())
    .bind(revision_id.to_string())
    .execute(library.database.pool())
    .await
    .expect("insert output");
}

fn uuid_like() -> String {
    RecordingId::new().to_string()
}

// --------------------------------------------------------- property tests --

mod properties {
    use super::*;
    use proptest::prelude::*;

    #[derive(Debug, Clone, Copy)]
    enum Tick {
        Listed,
        VersionBumped,
        Trashed,
        Removed,
        BytesChanged,
    }

    fn ticks() -> impl Strategy<Value = Vec<Tick>> {
        proptest::collection::vec(
            prop_oneof![
                Just(Tick::Listed),
                Just(Tick::VersionBumped),
                Just(Tick::Trashed),
                Just(Tick::Removed),
                Just(Tick::BytesChanged),
            ],
            1..12,
        )
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(10))]

        /// App. A.2: reconciliation converges. A second and third pass over an
        /// unchanged listing agree, and neither adds a row.
        #[test]
        fn repeated_reconciliation_is_idempotent(version in "v[0-9]{1,2}") {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("runtime");
            runtime.block_on(async move {
                let dir = tempfile::tempdir().expect("tempdir");
                let library = Library::open(dir.path()).await;
                let identity = cloud_identity("account-a", "rec-1");
                library.import(&identity, &wav_bytes(1, 512), 10).await;
                let source_id = library.source_id(&identity).await;
                let listing = vec![observed(&identity, &version)
                    .with_observed_content(Sha256Digest::of(&wav_bytes(1, 512)).to_hex())];

                let _settling = library
                    .reconcile
                    .reconcile_listing(source_id, &listing, Timestamp::from_millis(20))
                    .await
                    .expect("pass 1");
                let second = library
                    .reconcile
                    .reconcile_listing(source_id, &listing, Timestamp::from_millis(30))
                    .await
                    .expect("pass 2");
                let rows_after_second = (
                    library.count("recordings").await,
                    library.count("source_items").await,
                    library.count("media_assets").await,
                    library.count("media_revisions").await,
                );
                let third = library
                    .reconcile
                    .reconcile_listing(source_id, &listing, Timestamp::from_millis(40))
                    .await
                    .expect("pass 3");
                let rows_after_third = (
                    library.count("recordings").await,
                    library.count("source_items").await,
                    library.count("media_assets").await,
                    library.count("media_revisions").await,
                );

                prop_assert_eq!(second, third);
                prop_assert_eq!(rows_after_second, rows_after_third);
                Ok(())
            })?;
        }

        /// §7.10: no sequence of source events resurrects a deleted recording.
        #[test]
        fn no_event_sequence_resurrects_a_tombstoned_recording(script in ticks()) {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("runtime");
            runtime.block_on(async move {
                let dir = tempfile::tempdir().expect("tempdir");
                let library = Library::open(dir.path()).await;
                let identity = cloud_identity("account-a", "rec-1");
                library.import(&identity, &wav_bytes(1, 512), 10).await;
                let source_id = library.source_id(&identity).await;
                library
                    .reconcile
                    .record_local_deletion(&identity, "user deleted it", Timestamp::from_millis(20))
                    .await
                    .expect("tombstone");
                let recordings_before = library.count("recordings").await;

                let mut version = 1;
                for (index, tick) in script.into_iter().enumerate() {
                    let at = Timestamp::from_millis(30 + i64::try_from(index).unwrap_or(0));
                    let listing = match tick {
                        Tick::Listed => vec![observed(&identity, "v1")],
                        Tick::VersionBumped => {
                            version += 1;
                            vec![observed(&identity, &format!("v{version}"))]
                        }
                        Tick::Trashed => {
                            vec![observed(&identity, "v1").with_remote_state(RemoteState::Trashed)]
                        }
                        Tick::Removed => Vec::new(),
                        Tick::BytesChanged => vec![observed(&identity, "v9")
                            .with_observed_content(Sha256Digest::of(&wav_bytes(3, 7)).to_hex())],
                    };
                    let decisions = library
                        .reconcile
                        .reconcile_listing(source_id, &listing, at)
                        .await
                        .expect("reconcile");
                    for decision in decisions {
                        prop_assert_eq!(decision.event, SourceEvent::LocallyDeleted);
                    }
                    let outcome = library.import(&identity, &wav_bytes(1, 512), at.as_millis()).await;
                    let refused = matches!(outcome, ImportOutcome::RefusedTombstoned { .. });
                    prop_assert!(refused, "a tombstoned item must never re-import");
                }

                prop_assert_eq!(library.count("recordings").await, recordings_before);
                prop_assert_eq!(library.count("media_revisions").await, 0);
                Ok(())
            })?;
        }
    }
}
