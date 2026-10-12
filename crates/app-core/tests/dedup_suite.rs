//! The spec 18.6A source-reconciliation cluster and the spec 1.5 replay and
//! rename fixtures: same bytes from two meetings, different bytes with the
//! same filename, and a local tombstone that must not be resurrected.

use plaudon_app_core::import::{ImportOutcome, ImportRequest, ImportService};
use plaudon_domain::media::QualityClass;
use plaudon_domain::source::{
    ExternalRecordingId, SourceAccountScope, SourceIdentityKey, SourceKind,
};
use plaudon_domain::time::Timestamp;
use plaudon_persistence::archive::MediaArchive;
use plaudon_persistence::db::Database;
use plaudon_persistence::migrations::MigrationRunner;

struct Harness {
    _dir: tempfile::TempDir,
    service: ImportService,
}

async fn harness() -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(dir.path().join("app.sqlite")).await.unwrap();
    MigrationRunner::new().run(&db).await.unwrap();
    let archive = MediaArchive::open(dir.path().join("archive")).unwrap();
    Harness {
        _dir: dir,
        service: ImportService::new(db, archive),
    }
}

fn cloud_item(scope: &str, external: &str) -> SourceIdentityKey {
    SourceIdentityKey::new(
        SourceKind::PlaudCloudCli,
        SourceAccountScope::new(scope),
        Some(ExternalRecordingId::new(external)),
    )
}

fn request(identity: SourceIdentityKey, name: &str) -> ImportRequest {
    ImportRequest {
        identity,
        display_name: name.into(),
        quality_class: QualityClass::VendorCloudExport,
        extension: "wav".into(),
        observed_codec: "pcm_s16le".into(),
        remote_version: Some("v1".into()),
        attach_to_recording: None,
    }
}

#[tokio::test]
async fn identical_bytes_from_two_sources_share_one_blob_and_stay_two_recordings() {
    let h = harness().await;
    let bytes = b"the same meeting audio";

    let first = h
        .service
        .import(
            &request(cloud_item("account-a", "rec-1"), "Account A copy"),
            &mut std::io::Cursor::new(bytes.to_vec()),
            Timestamp::from_millis(1),
        )
        .await
        .unwrap();
    let second = h
        .service
        .import(
            &request(cloud_item("account-b", "rec-1"), "Account B copy"),
            &mut std::io::Cursor::new(bytes.to_vec()),
            Timestamp::from_millis(2),
        )
        .await
        .unwrap();

    let (
        ImportOutcome::Imported {
            recording_id: first_id,
            media_asset_id: first_asset,
            reused_existing_blob: first_reused,
            ..
        },
        ImportOutcome::Imported {
            recording_id: second_id,
            media_asset_id: second_asset,
            reused_existing_blob: second_reused,
            ..
        },
    ) = (first, second)
    else {
        panic!("both imports must create recordings");
    };

    assert_ne!(
        first_id, second_id,
        "the same external id in two accounts is two meetings"
    );
    assert_eq!(first_asset, second_asset, "identical bytes share one blob");
    assert!(!first_reused);
    assert!(second_reused);
    assert_eq!(h.service.assets().count().await.unwrap(), 1);
    assert_eq!(h.service.journal().archive().list_blobs().unwrap().len(), 1);

    // Independent user-owned state.
    let mut a = h.service.recordings().get(first_id).await.unwrap();
    a.title = "Quarterly planning".into();
    a.tags.insert("planning".into());
    a.annotation = Some("Alice chaired".into());
    h.service.recordings().update_user_fields(&a).await.unwrap();

    let b = h.service.recordings().get(second_id).await.unwrap();
    assert_eq!(b.title, "Account B copy");
    assert!(
        b.tags.is_empty(),
        "expected b.tags to be empty, got {:?}",
        b.tags
    );
    assert_eq!(b.annotation, None);
    assert_eq!(
        h.service.recordings().get(first_id).await.unwrap().title,
        "Quarterly planning"
    );
}

#[tokio::test]
async fn re_importing_the_same_source_item_is_idempotent() {
    let h = harness().await;
    let identity = cloud_item("account-a", "rec-1");

    let first = h
        .service
        .import(
            &request(identity.clone(), "Standup"),
            &mut std::io::Cursor::new(b"audio".to_vec()),
            Timestamp::from_millis(1),
        )
        .await
        .unwrap();
    let ImportOutcome::Imported {
        recording_id,
        source_item_id,
        ..
    } = first
    else {
        panic!("first import must create a recording");
    };

    for tick in 2..6 {
        let repeat = h
            .service
            .import(
                &request(identity.clone(), "Standup"),
                &mut std::io::Cursor::new(b"audio".to_vec()),
                Timestamp::from_millis(tick),
            )
            .await
            .unwrap();
        assert_eq!(
            repeat,
            ImportOutcome::AlreadyImported {
                recording_id,
                source_item_id
            },
            "re-import must not create a second recording"
        );
    }
    assert_eq!(h.service.assets().count().await.unwrap(), 1);
    assert_eq!(h.service.journal().archive().list_blobs().unwrap().len(), 1);
}

#[tokio::test]
async fn the_same_filename_with_different_bytes_yields_two_assets() {
    let h = harness().await;
    for (index, bytes) in [b"version one".as_slice(), b"version two".as_slice()]
        .iter()
        .enumerate()
    {
        h.service
            .import(
                &request(
                    cloud_item("account-a", &format!("rec-{index}")),
                    "meeting.wav",
                ),
                &mut std::io::Cursor::new(bytes.to_vec()),
                Timestamp::from_millis(i64::try_from(index).unwrap() + 1),
            )
            .await
            .unwrap();
    }
    assert_eq!(h.service.assets().count().await.unwrap(), 2);
    assert_eq!(h.service.journal().archive().list_blobs().unwrap().len(), 2);
}

#[tokio::test]
async fn different_filenames_with_the_same_bytes_yield_one_asset() {
    let h = harness().await;
    for (index, name) in ["monday.wav", "monday-copy.wav"].iter().enumerate() {
        h.service
            .import(
                &request(cloud_item("account-a", &format!("rec-{index}")), name),
                &mut std::io::Cursor::new(b"identical".to_vec()),
                Timestamp::from_millis(i64::try_from(index).unwrap() + 1),
            )
            .await
            .unwrap();
    }
    assert_eq!(h.service.assets().count().await.unwrap(), 1);
}

#[tokio::test]
async fn a_higher_fidelity_candidate_never_overwrites_the_existing_one() {
    let h = harness().await;
    let first = h
        .service
        .import(
            &ImportRequest {
                quality_class: QualityClass::CompressedDeviceTransfer,
                extension: "asr".into(),
                ..request(cloud_item("device-1", "rec-1"), "REC0001")
            },
            &mut std::io::Cursor::new(b"compressed sidecar".to_vec()),
            Timestamp::from_millis(1),
        )
        .await
        .unwrap();
    let ImportOutcome::Imported {
        recording_id,
        media_asset_id: compressed,
        ..
    } = first
    else {
        panic!("expected an import");
    };

    let second = h
        .service
        .import(
            &ImportRequest {
                quality_class: QualityClass::OriginalDeviceRecording,
                extension: "wav".into(),
                attach_to_recording: Some(recording_id),
                ..request(cloud_item("device-1", "rec-1-wav"), "REC0001.WAV")
            },
            &mut std::io::Cursor::new(b"original high fidelity wav".to_vec()),
            Timestamp::from_millis(2),
        )
        .await
        .unwrap();
    let ImportOutcome::MediaCandidateAdded {
        media_asset_id: original,
        ..
    } = second
    else {
        panic!("expected a candidate to be added");
    };

    let recording = h.service.recordings().get(recording_id).await.unwrap();
    assert_eq!(recording.media_candidates().len(), 2);
    assert_eq!(
        recording.preferred_media_asset_id(),
        Some(compressed),
        "promotion must be an explicit action"
    );

    h.service
        .recordings()
        .set_preferred_media(recording_id, original)
        .await
        .unwrap();
    let recording = h.service.recordings().get(recording_id).await.unwrap();
    assert_eq!(recording.preferred_media_asset_id(), Some(original));
    assert_eq!(
        recording.media_candidates().len(),
        2,
        "promotion keeps both candidates"
    );
    assert_eq!(h.service.assets().count().await.unwrap(), 2);
}

#[tokio::test]
async fn a_tombstoned_recording_is_not_resurrected_by_a_repeat_import() {
    let h = harness().await;
    let identity = cloud_item("account-a", "rec-1");
    let ImportOutcome::Imported { recording_id, .. } = h
        .service
        .import(
            &request(identity.clone(), "Standup"),
            &mut std::io::Cursor::new(b"audio".to_vec()),
            Timestamp::from_millis(1),
        )
        .await
        .unwrap()
    else {
        panic!("expected an import");
    };

    h.service
        .recordings()
        .tombstone(
            recording_id,
            "user deleted",
            Timestamp::from_millis(5),
            None,
        )
        .await
        .unwrap();

    let repeat = h
        .service
        .import(
            &request(identity, "Standup"),
            &mut std::io::Cursor::new(b"audio".to_vec()),
            Timestamp::from_millis(6),
        )
        .await
        .unwrap();
    assert_eq!(repeat, ImportOutcome::RefusedTombstoned { recording_id });
    assert_eq!(
        h.service.assets().count().await.unwrap(),
        1,
        "no new blob was stored"
    );
}

#[tokio::test]
async fn a_corrupt_existing_blob_fails_the_import_instead_of_being_reused() {
    let h = harness().await;
    h.service
        .import(
            &request(cloud_item("account-a", "rec-1"), "Standup"),
            &mut std::io::Cursor::new(b"audio".to_vec()),
            Timestamp::from_millis(1),
        )
        .await
        .unwrap();

    let blobs = h.service.journal().archive().list_blobs().unwrap();
    let path = h.service.journal().archive().absolute(&blobs[0].0).unwrap();
    std::fs::write(path, b"corrupted on disk").unwrap();

    let error = h
        .service
        .import(
            &request(cloud_item("account-b", "rec-1"), "Same audio elsewhere"),
            &mut std::io::Cursor::new(b"audio".to_vec()),
            Timestamp::from_millis(2),
        )
        .await
        .unwrap_err();
    assert_eq!(
        error.code(),
        plaudon_domain::error::AppErrorCode::IntegrityFailed
    );
}

#[tokio::test]
async fn a_remote_metadata_change_updates_only_the_source_association() {
    let h = harness().await;
    let identity = cloud_item("account-a", "rec-1");
    let ImportOutcome::Imported {
        recording_id,
        source_item_id,
        media_asset_id,
        ..
    } = h
        .service
        .import(
            &request(identity.clone(), "Standup"),
            &mut std::io::Cursor::new(b"audio".to_vec()),
            Timestamp::from_millis(1),
        )
        .await
        .unwrap()
    else {
        panic!("expected an import");
    };

    let renamed = ImportRequest {
        remote_version: Some("v2".into()),
        ..request(identity.clone(), "Standup (renamed at source)")
    };
    let outcome = h
        .service
        .import(
            &renamed,
            &mut std::io::Cursor::new(b"audio".to_vec()),
            Timestamp::from_millis(9),
        )
        .await
        .unwrap();
    assert_eq!(
        outcome,
        ImportOutcome::AlreadyImported {
            recording_id,
            source_item_id
        }
    );

    let source_id = h
        .service
        .source_id(&identity, Timestamp::from_millis(9))
        .await
        .unwrap();
    let association = h
        .service
        .sources()
        .find_by_identity(source_id, &identity)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(association.remote_version.as_deref(), Some("v2"));

    let recording = h.service.recordings().get(recording_id).await.unwrap();
    assert_eq!(
        recording.title, "Standup",
        "a source rename never rewrites user-owned state"
    );
    assert_eq!(recording.preferred_media_asset_id(), Some(media_asset_id));
    h.service
        .journal()
        .archive()
        .verify(
            &h.service
                .assets()
                .get(media_asset_id)
                .await
                .unwrap()
                .relative_path,
            &h.service
                .assets()
                .get(media_asset_id)
                .await
                .unwrap()
                .identity,
        )
        .unwrap();
}
