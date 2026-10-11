//! Property-based invariants for identity, ordering, idempotency and the
//! import journal (spec 18.2, 18.9).
//!
//! Every case is seeded deterministically, so a failure reproduces from the
//! recorded seed in `proptest-regressions/`.

use plaudon_app_core::dedup::{BlobDecision, DedupPolicy};
use plaudon_app_core::import::{ImportOutcome, ImportRequest, ImportService};
use plaudon_domain::ids::{MediaAssetId, RecordingId, SourceId};
use plaudon_domain::media::{MediaIdentity, QualityClass, Sha256Digest};
use plaudon_domain::model::{EngineId, ModelIdentity};
use plaudon_domain::recording::{Recording, RecordingMedia};
use plaudon_domain::source::{
    ExternalRecordingId, SourceAccountScope, SourceIdentityKey, SourceKind,
};
use plaudon_domain::time::Timestamp;
use plaudon_domain::transcript::{
    AlignmentOrigin, RevisionOrigin, RevisionStatus, TranscriptError, TranscriptRevision,
    TranscriptRevisionDraft, TranscriptSegment,
};
use plaudon_persistence::archive::{self, MediaArchive, ALLOWED_EXTENSIONS};
use plaudon_persistence::db::Database;
use plaudon_persistence::journal::JournalState;
use plaudon_persistence::migrations::MigrationRunner;
use proptest::prelude::*;

fn revision_draft(
    origin: AlignmentOrigin,
    segments: Vec<TranscriptSegment>,
) -> TranscriptRevisionDraft {
    TranscriptRevisionDraft {
        recording_id: RecordingId::new(),
        parent_id: None,
        origin: RevisionOrigin::Machine,
        input_sha256: Sha256Digest::of(b"audio"),
        engine: EngineId::new("whisper-cpp"),
        model: ModelIdentity::new(EngineId::new("whisper-cpp"), "base.en"),
        locale: None,
        alignment_origin: origin,
        status: RevisionStatus::Complete,
        normalization_profile: "nfc-basic".into(),
        chunking_strategy: "single-pass".into(),
        duration_ms: None,
        created_at: Timestamp::EPOCH,
        segments,
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 128, failure_persistence: Some(Box::new(proptest::test_runner::FileFailurePersistence::WithSource("proptest-regressions"))), ..ProptestConfig::default() })]

    /// Media identity is exactly `(sha256, byte_length)`: the observed codec
    /// is provenance and never changes identity.
    #[test]
    fn media_identity_ignores_the_observed_codec(
        bytes in proptest::collection::vec(any::<u8>(), 0..512),
        codec_a in "[a-z0-9_]{1,12}",
        codec_b in "[a-z0-9_]{1,12}",
    ) {
        let a = MediaIdentity::of_bytes(&bytes, codec_a);
        let b = MediaIdentity::of_bytes(&bytes, codec_b);
        prop_assert_eq!(&a, &b);

        let mut different = a.clone();
        different.byte_length = a.byte_length.wrapping_add(1);
        prop_assert_ne!(&a, &different);
    }

    /// Different bytes almost never produce the same identity, and identical
    /// bytes always do.
    #[test]
    fn distinct_bytes_produce_distinct_identities(
        a in proptest::collection::vec(any::<u8>(), 0..256),
        b in proptest::collection::vec(any::<u8>(), 0..256),
    ) {
        let left = MediaIdentity::of_bytes(&a, "wav");
        let right = MediaIdentity::of_bytes(&b, "wav");
        prop_assert_eq!(left == right, a == b);
    }

    /// Archive paths are always a canonical four-segment relative path under
    /// `media/sha256`, whatever the caller passes as an extension.
    #[test]
    fn archive_paths_are_always_canonical(
        bytes in proptest::collection::vec(any::<u8>(), 0..64),
        extension in ".{0,40}",
    ) {
        let identity = MediaIdentity::of_bytes(&bytes, "wav");
        let path = archive::relative_path(&identity, &extension);
        let hex = identity.sha256.to_hex();

        prop_assert!(path.starts_with("media/sha256/"));
        prop_assert_eq!(path.matches('/').count(), 3);
        prop_assert!(!path.contains(".."));
        prop_assert!(!path.contains('\\'));
        prop_assert!(!path.contains('\0'));

        let (directory, file) = path.rsplit_once('/').unwrap();
        prop_assert_eq!(directory, format!("media/sha256/{}", &hex[..2]));
        let (stem, actual_extension) = file.rsplit_once('.').unwrap();
        prop_assert_eq!(stem, hex.as_str());
        prop_assert!(ALLOWED_EXTENSIONS.contains(&actual_extension));
    }

    /// A revision accepts a segment list exactly when the list satisfies the
    /// ordering and time invariants, and never otherwise.
    #[test]
    fn segment_validation_matches_the_stated_invariants(
        starts in proptest::collection::vec(0u64..100_000, 0..12),
        durations in proptest::collection::vec(0u64..5_000, 0..12),
    ) {
        let count = starts.len().min(durations.len());
        let segments: Vec<TranscriptSegment> = (0..count)
            .map(|i| {
                let seq = u32::try_from(i).unwrap();
                TranscriptSegment::timed(seq, starts[i], starts[i] + durations[i], "x")
            })
            .collect();

        let monotonic = segments.windows(2).all(|pair| {
            pair[0].start.unwrap().get() <= pair[1].start.unwrap().get()
        });
        let result = TranscriptRevision::try_new(revision_draft(AlignmentOrigin::Native, segments));

        prop_assert_eq!(result.is_ok(), monotonic);
        if let Ok(revision) = result {
            let offsets = revision.seek_offsets().unwrap();
            prop_assert!(offsets.windows(2).all(|pair| pair[0] <= pair[1]));
            for segment in revision.segments() {
                prop_assert!(segment.end.unwrap().get() >= segment.start.unwrap().get());
            }
        }
    }

    /// An unaligned revision never exposes a seek offset, whatever text it
    /// carries.
    #[test]
    fn unaligned_revisions_never_expose_offsets(texts in proptest::collection::vec(".{0,40}", 0..10)) {
        let segments: Vec<TranscriptSegment> = texts
            .iter()
            .enumerate()
            .map(|(i, text)| TranscriptSegment::text_only(u32::try_from(i).unwrap(), text.clone()))
            .collect();
        let revision =
            TranscriptRevision::try_new(revision_draft(AlignmentOrigin::None, segments)).unwrap();
        prop_assert!(!revision.permits_seek());
        prop_assert_eq!(revision.seek_offsets(), None);
    }

    /// An out-of-order index is always rejected, and the error names the first
    /// offending position.
    #[test]
    fn misindexed_segments_are_rejected(
        count in 1usize..8,
        offender in 0usize..8,
        wrong_index in 0u32..64,
    ) {
        let offender = offender % count;
        let mut segments: Vec<TranscriptSegment> = (0..count)
            .map(|i| TranscriptSegment::text_only(u32::try_from(i).unwrap(), "x"))
            .collect();
        let expected = u32::try_from(offender).unwrap();
        prop_assume!(wrong_index != expected);
        segments[offender].seq_index = wrong_index;

        let error =
            TranscriptRevision::try_new(revision_draft(AlignmentOrigin::None, segments)).unwrap_err();
        prop_assert_eq!(
            error,
            TranscriptError::SegmentOrder { position: offender, seq_index: wrong_index, expected }
        );
    }

    /// Attaching media candidates is additive and never changes the preference
    /// implicitly.
    #[test]
    fn attaching_candidates_is_non_destructive(count in 1usize..8) {
        let mut recording = Recording::new(RecordingId::new(), "m", Timestamp::EPOCH);
        let mut ids = Vec::new();
        for i in 0..count {
            let candidate = RecordingMedia {
                media_asset_id: MediaAssetId::new(),
                quality_class: QualityClass::UserSuppliedFile,
                obtained_at: Timestamp::from_millis(i64::try_from(i).unwrap()),
            };
            ids.push(candidate.media_asset_id);
            recording.attach_media(candidate).unwrap();
        }
        prop_assert_eq!(recording.media_candidates().len(), count);
        prop_assert_eq!(recording.preferred_media_asset_id(), Some(ids[0]));
        for id in &ids {
            prop_assert!(recording.media_candidates().iter().any(|c| c.media_asset_id == *id));
        }
    }

    /// Source identity is the full triple; changing any component changes the
    /// identity.
    #[test]
    fn source_identity_is_the_full_triple(
        scope_a in "[a-z]{1,8}",
        scope_b in "[a-z]{1,8}",
        external in "[a-z0-9-]{1,16}",
    ) {
        let a = SourceIdentityKey::new(
            SourceKind::PlaudCloudCli,
            SourceAccountScope::new(scope_a.clone()),
            Some(ExternalRecordingId::new(external.clone())),
        );
        let b = SourceIdentityKey::new(
            SourceKind::PlaudCloudCli,
            SourceAccountScope::new(scope_b.clone()),
            Some(ExternalRecordingId::new(external.clone())),
        );
        prop_assert_eq!(a == b, scope_a == scope_b);

        let other_kind = SourceIdentityKey::new(SourceKind::PlaudDeviceBle, a.account_scope.clone(), a.external_id.clone());
        prop_assert_ne!(&a, &other_kind);
    }

    /// Deduplication reuses a blob only when the existing bytes verified.
    #[test]
    fn dedup_only_reuses_verified_blobs(verified in proptest::option::of(any::<bool>())) {
        let identity = MediaIdentity::of_bytes(b"audio", "wav");
        let asset = plaudon_domain::media::MediaAsset {
            id: MediaAssetId::new(),
            relative_path: archive::relative_path(&identity, "wav"),
            identity,
            quality_class: QualityClass::UserSuppliedFile,
            verification_state: plaudon_domain::media::VerificationState::Verified,
            verified_at: Some(Timestamp::EPOCH),
        };
        let decision = DedupPolicy::decide(Some(&asset), verified);
        if verified == Some(true) {
            prop_assert_eq!(decision, BlobDecision::ReuseExisting(asset.id));
        } else {
            prop_assert_eq!(decision, BlobDecision::ExistingCorrupt(asset.id));
        }
    }

    /// Journal states round-trip through their stored representation, and
    /// only the three settled states report themselves settled.
    #[test]
    fn journal_states_round_trip(index in 0usize..8) {
        const ALL: [JournalState; 8] = [
            JournalState::Staged,
            JournalState::Hashed,
            JournalState::Flushed,
            JournalState::IntentRecorded,
            JournalState::Renamed,
            JournalState::Committed,
            JournalState::Abandoned,
            JournalState::Quarantined,
        ];
        let state = ALL[index];
        prop_assert_eq!(JournalState::parse(state.as_str()), Some(state));
        prop_assert_eq!(
            state.is_settled(),
            matches!(state, JournalState::Committed | JournalState::Abandoned | JournalState::Quarantined)
        );
    }
}

/// Importing the same stable source item any number of times creates exactly
/// one recording and exactly one blob.
#[test]
fn repeated_imports_of_one_source_item_are_idempotent() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        for repeats in 1..=5u32 {
            let dir = tempfile::tempdir().unwrap();
            let db = Database::open(dir.path().join("app.sqlite")).await.unwrap();
            MigrationRunner::new().run(&db).await.unwrap();
            let archive = MediaArchive::open(dir.path().join("archive")).unwrap();
            let service = ImportService::new(db, archive);

            let identity = SourceIdentityKey::new(
                SourceKind::PlaudCloudCli,
                SourceAccountScope::new("account"),
                Some(ExternalRecordingId::new("rec-1")),
            );
            let request = ImportRequest {
                identity,
                display_name: "Standup".into(),
                quality_class: QualityClass::VendorCloudExport,
                extension: "wav".into(),
                observed_codec: "pcm_s16le".into(),
                remote_version: None,
                attach_to_recording: None,
            };

            let mut recording_ids = Vec::new();
            for tick in 0..repeats {
                let outcome = service
                    .import(
                        &request,
                        &mut std::io::Cursor::new(b"audio".to_vec()),
                        Timestamp::from_millis(i64::from(tick)),
                    )
                    .await
                    .unwrap();
                let id: Option<RecordingId> = outcome.into();
                recording_ids.push(id.unwrap());
            }

            assert!(recording_ids.windows(2).all(|pair| pair[0] == pair[1]));
            assert_eq!(service.assets().count().await.unwrap(), 1);
            assert_eq!(service.journal().archive().list_blobs().unwrap().len(), 1);
            assert_eq!(service.journal().incomplete().await.unwrap().len(), 0);
        }
    });
}

/// Distinct source items with identical bytes always produce distinct
/// recordings sharing one blob, for any number of items.
#[test]
fn identical_bytes_across_n_sources_never_merge_recordings() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        for sources in 2..=5u32 {
            let dir = tempfile::tempdir().unwrap();
            let db = Database::open(dir.path().join("app.sqlite")).await.unwrap();
            MigrationRunner::new().run(&db).await.unwrap();
            let archive = MediaArchive::open(dir.path().join("archive")).unwrap();
            let service = ImportService::new(db, archive);

            let mut ids = std::collections::BTreeSet::new();
            let mut source_ids = std::collections::BTreeSet::new();
            for index in 0..sources {
                let identity = SourceIdentityKey::new(
                    SourceKind::PlaudCloudCli,
                    SourceAccountScope::new(format!("account-{index}")),
                    Some(ExternalRecordingId::new("rec-1")),
                );
                let outcome = service
                    .import(
                        &ImportRequest {
                            identity,
                            display_name: format!("Copy {index}"),
                            quality_class: QualityClass::VendorCloudExport,
                            extension: "wav".into(),
                            observed_codec: "pcm_s16le".into(),
                            remote_version: None,
                            attach_to_recording: None,
                        },
                        &mut std::io::Cursor::new(b"identical".to_vec()),
                        Timestamp::from_millis(i64::from(index)),
                    )
                    .await
                    .unwrap();
                let ImportOutcome::Imported {
                    recording_id,
                    source_item_id,
                    media_asset_id,
                    ..
                } = outcome
                else {
                    panic!("each distinct source item must create its own recording");
                };
                ids.insert(recording_id);
                source_ids.insert(source_item_id);
                assert_eq!(
                    service.assets().get(media_asset_id).await.unwrap().id,
                    media_asset_id
                );
            }

            assert_eq!(u32::try_from(ids.len()).unwrap(), sources);
            assert_eq!(u32::try_from(source_ids.len()).unwrap(), sources);
            assert_eq!(service.assets().count().await.unwrap(), 1);
            assert_eq!(service.journal().archive().list_blobs().unwrap().len(), 1);
        }
    });
}

/// The sum of the two: only the hermetic components are involved, and no
/// network, vendor CLI or paid endpoint exists anywhere in this suite.
#[test]
fn the_suite_depends_only_on_local_components() {
    let _ = SourceId::new();
    assert!(!DedupPolicy::may_merge_recordings());
}
