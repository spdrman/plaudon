//! §18.6 watcher fixtures and App. A.4 hostile inputs (tasks K2.3, K2.4, K2.5).
//!
//! Every scenario in §18.6's watcher list has a test here, and every one of
//! them ends by proving the source file is byte-identical. That assertion is
//! not decoration: §4.3(4) and §22(11) forbid renaming, moving, truncating or
//! deleting a user's file, and a watcher is exactly the component most likely
//! to do it by accident.
//!
//! The suite is hermetic. Time is a manual clock, so nothing sleeps; the two
//! conditions a filesystem will not produce on request — a cloud placeholder
//! and a full volume — are injected probes.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use plaudon_domain::media::Sha256Digest;
use plaudon_integration_tests::watcher::{
    append_file, assert_tree_unchanged, read_file, snapshot_tree, wav_bytes, write_by_atomic_rename,
    write_file, SourceSnapshot, WatcherHarness, QUIESCENCE_MS,
};
use plaudon_security::consent::{
    ConsentGrant, ConsentPurpose, ContentType, DispatchRequest, GrantScope, ProviderRoute,
    RetentionPosture,
};
use plaudon_security::outbound::OutboundPolicy;
use plaudon_source_folder::barrier::{BarrierOutcome, BarrierPolicy, DeferReason};
use plaudon_source_folder::config::FolderRule;
use plaudon_source_folder::fsmeta::FixedDiskSpaceProbe;
use plaudon_source_folder::placeholder::{PlaceholderReason, PlaceholderState};
use plaudon_source_folder::queue::{CoalescingQueue, OfferOutcome};
use plaudon_source_folder::service::{CandidateOutcome, QueueingLocalWorkSink};
use plaudon_source_folder::watcher::{FsHint, ScriptedEventSource};

// ---------------------------------------------------------------- helpers --

async fn recording_count(harness: &WatcherHarness) -> i64 {
    sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM recordings")
        .fetch_one(harness.database.pool())
        .await
        .expect("count recordings")
}

async fn media_asset_count(harness: &WatcherHarness) -> i64 {
    sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM media_assets")
        .fetch_one(harness.database.pool())
        .await
        .expect("count assets")
}

fn imported_recording(outcome: &CandidateOutcome) -> bool {
    matches!(outcome, CandidateOutcome::Imported { .. })
}

/// Make a path unreadable, reporting whether the platform actually enforced
/// it. A suite running as root cannot be denied, and a test must say so
/// rather than pretend it proved something.
#[cfg(unix)]
fn make_unreadable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path).expect("stat").permissions();
    perms.set_mode(0o000);
    std::fs::set_permissions(path, perms).expect("chmod");
    std::fs::File::open(path).is_err() || std::fs::read_dir(path).is_err()
}

#[cfg(unix)]
fn restore_readable(path: &Path, dir: bool) {
    use std::os::unix::fs::PermissionsExt;
    let mode = if dir { 0o755 } else { 0o644 };
    let mut perms = std::fs::metadata(path).expect("stat").permissions();
    perms.set_mode(mode);
    std::fs::set_permissions(path, perms).expect("chmod");
}

// ------------------------------------------------- §18.6: late writes ------

#[tokio::test]
async fn a_file_still_being_written_is_never_imported_until_it_settles() {
    let dir = tempfile::tempdir().expect("tempdir");
    let harness = WatcherHarness::open(dir.path(), FolderRule::ImportOnly).await;
    let path = harness.path("late.wav");
    let full = wav_bytes(7, 4_000);

    // The writer has produced a valid-looking prefix and is not done.
    write_file(&path, &full[..1_500]);
    let first = harness.process(&path).await;
    assert!(
        matches!(first, CandidateOutcome::Deferred { .. }),
        "a creation event is not a complete transfer: {first:?}"
    );
    assert_eq!(recording_count(&harness).await, 0);

    // A late write inside the window restarts it.
    harness.advance(10);
    append_file(&path, &full[1_500..]);
    let second = harness.process(&path).await;
    assert!(matches!(second, CandidateOutcome::Deferred { .. }));
    assert_eq!(recording_count(&harness).await, 0);

    let snapshot = SourceSnapshot::capture(&path);
    harness.advance(i64::try_from(QUIESCENCE_MS).unwrap() + 1);
    let settled = harness.process(&path).await;
    assert!(imported_recording(&settled), "unexpected: {settled:?}");

    match settled {
        CandidateOutcome::Imported { content_sha256, .. } => {
            assert_eq!(content_sha256, Sha256Digest::of(&full).to_hex());
        }
        other => panic!("expected an import, got {other:?}"),
    }
    snapshot.assert_unchanged();
}

// --------------------------------------- §18.6: writes by atomic rename ----

#[tokio::test]
async fn a_file_published_by_atomic_rename_imports_once_and_is_left_alone() {
    let dir = tempfile::tempdir().expect("tempdir");
    let harness = WatcherHarness::open(dir.path(), FolderRule::ImportOnly).await;
    let path = harness.path("atomic.wav");
    write_by_atomic_rename(&path, &wav_bytes(3, 2_048));
    let snapshot = SourceSnapshot::capture(&path);

    let outcome = harness.settle(&path).await;
    assert!(imported_recording(&outcome), "unexpected: {outcome:?}");
    assert_eq!(recording_count(&harness).await, 1);

    // A second pass over the same file is not a second recording.
    let again = harness.settle(&path).await;
    assert!(matches!(again, CandidateOutcome::AlreadyImported { .. }));
    assert_eq!(recording_count(&harness).await, 1);

    snapshot.assert_unchanged();
    // The staging sibling the publisher used must not have been left behind
    // or adopted.
    assert!(!path.with_extension("partial-write").exists());
}

// ------------------------------- §18.6: content replacement at same path ---

#[tokio::test]
async fn content_replaced_at_the_same_path_is_re_examined_not_assumed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let harness = WatcherHarness::open(dir.path(), FolderRule::ImportOnly).await;
    let path = harness.path("replaced.wav");

    write_file(&path, &wav_bytes(1, 2_000));
    let first = harness.settle(&path).await;
    assert!(imported_recording(&first), "unexpected: {first:?}");
    let first_sha = match &first {
        CandidateOutcome::Imported { content_sha256, .. } => content_sha256.clone(),
        other => panic!("expected an import, got {other:?}"),
    };

    // Replaced by a different publisher: new inode, different bytes.
    harness.advance(1_000);
    write_by_atomic_rename(&path, &wav_bytes(9, 3_000));
    let snapshot = SourceSnapshot::capture(&path);
    let second = harness.settle(&path).await;
    assert!(
        imported_recording(&second),
        "a replaced file is a new candidate: {second:?}"
    );
    let second_sha = match &second {
        CandidateOutcome::Imported { content_sha256, .. } => content_sha256.clone(),
        other => panic!("expected an import, got {other:?}"),
    };
    assert_ne!(first_sha, second_sha);
    assert_eq!(
        media_asset_count(&harness).await,
        2,
        "two distinct contents are two blobs"
    );
    snapshot.assert_unchanged();
}

// ------------------------------------------------- §18.6: locked files -----

#[cfg(unix)]
#[tokio::test]
async fn a_file_that_cannot_be_read_is_deferred_and_left_untouched() {
    let dir = tempfile::tempdir().expect("tempdir");
    let harness = WatcherHarness::open(dir.path(), FolderRule::ImportOnly).await;
    let path = harness.path("locked.wav");
    write_file(&path, &wav_bytes(5, 1_024));
    let snapshot = SourceSnapshot::capture(&path);

    if !make_unreadable(&path) {
        restore_readable(&path, false);
        eprintln!("skipping: this process cannot be denied read access");
        return;
    }

    let outcome = harness.settle(&path).await;
    match &outcome {
        CandidateOutcome::Deferred { reason, .. } => {
            assert!(matches!(reason, DeferReason::Unreadable(_)), "{reason:?}");
            assert!(reason.is_retryable(), "a lock is temporary");
        }
        other => panic!("a locked file must not import: {other:?}"),
    }
    assert_eq!(recording_count(&harness).await, 0);

    restore_readable(&path, false);
    snapshot.assert_unchanged();
}

// --------------------------------------- §18.6: permission revocation ------

#[cfg(unix)]
#[tokio::test]
async fn permission_revoked_on_a_subdirectory_is_surfaced_without_cleanup() {
    let dir = tempfile::tempdir().expect("tempdir");
    let harness = WatcherHarness::open_with(
        dir.path(),
        FolderRule::ImportOnly,
        Arc::new(plaudon_source_folder::fsmeta::OsDiskSpaceProbe),
        true,
    )
    .await;
    let nested = harness.root.join("restricted");
    std::fs::create_dir_all(&nested).expect("mkdir");
    let path = nested.join("inside.wav");
    write_file(&path, &wav_bytes(2, 1_024));
    let snapshot = SourceSnapshot::capture(&path);

    if !make_unreadable(&nested) {
        restore_readable(&nested, true);
        eprintln!("skipping: this process cannot be denied directory access");
        return;
    }

    // The sweep records the unreadable subtree and keeps going.
    let report = harness.service.run_inventory().await.expect("sweep");
    assert!(report.skipped >= 1, "{report:?}");
    assert_eq!(recording_count(&harness).await, 0);

    restore_readable(&nested, true);
    snapshot.assert_unchanged();
}

// --------------------------------- §18.6: inaccessible network share -------

#[cfg(unix)]
#[tokio::test]
async fn an_unreachable_share_defers_and_leaves_the_library_alone() {
    let dir = tempfile::tempdir().expect("tempdir");
    let harness = WatcherHarness::open_with(
        dir.path(),
        FolderRule::ImportOnly,
        Arc::new(plaudon_source_folder::fsmeta::OsDiskSpaceProbe),
        true,
    )
    .await;
    // A mounted share is a subdirectory that can stop answering.
    let share = harness.root.join("share");
    std::fs::create_dir_all(&share).expect("mkdir");
    let path = share.join("remote.wav");
    write_file(&path, &wav_bytes(4, 1_500));
    let snapshot = SourceSnapshot::capture(&path);

    if !make_unreadable(&share) {
        restore_readable(&share, true);
        eprintln!("skipping: this process cannot be denied directory access");
        return;
    }
    let outcome = harness.process(&path).await;
    assert!(
        !imported_recording(&outcome),
        "an unreachable share must not import: {outcome:?}"
    );
    assert_eq!(recording_count(&harness).await, 0);

    restore_readable(&share, true);
    snapshot.assert_unchanged();
}

// ------------------------------------------------ §18.6: cloud placeholders

#[tokio::test]
async fn a_cloud_placeholder_is_a_retryable_status_and_never_a_recording() {
    let dir = tempfile::tempdir().expect("tempdir");
    let harness = WatcherHarness::open(dir.path(), FolderRule::ImportOnly).await;
    let path = harness.path("evicted.wav");
    write_file(&path, &wav_bytes(6, 2_048));
    let snapshot = SourceSnapshot::capture(&path);

    // Dataless for the first two probes, hydrated afterwards.
    harness.placeholder.script(
        path.clone(),
        vec![
            PlaceholderState::Placeholder(PlaceholderReason::DatalessName),
            PlaceholderState::Placeholder(PlaceholderReason::NoAllocatedBlocks),
        ],
    );

    // The first probe only establishes the quiescence baseline; the
    // placeholder gate is reached once the window has elapsed.
    let baseline = harness.process(&path).await;
    assert!(matches!(baseline, CandidateOutcome::Deferred { .. }));
    harness.advance(i64::try_from(QUIESCENCE_MS).unwrap() + 1);
    let first = harness.process(&path).await;
    match &first {
        CandidateOutcome::Deferred { reason, .. } => {
            assert!(
                matches!(reason, DeferReason::CloudPlaceholder(_)),
                "{reason:?}"
            );
        }
        other => panic!("a placeholder must not import: {other:?}"),
    }
    assert_eq!(recording_count(&harness).await, 0);

    let second = harness.process(&path).await;
    assert!(matches!(second, CandidateOutcome::Deferred { .. }));
    assert_eq!(recording_count(&harness).await, 0);

    // The status is recorded, and the retry is counted (§17.4).
    let items = harness.service.items().await.expect("items");
    assert!(items.iter().any(|row| row.state == "placeholder"));
    assert_eq!(
        harness
            .service
            .metrics()
            .watcher_snapshot()
            .placeholder_retries,
        2
    );

    // Once the bytes are local the same path imports normally.
    let third = harness.process(&path).await;
    assert!(imported_recording(&third), "unexpected: {third:?}");
    snapshot.assert_unchanged();
}

// ------------------------------------------ §18.6: duplicate notifications -

#[tokio::test]
async fn a_burst_of_duplicate_notifications_produces_exactly_one_import() {
    let dir = tempfile::tempdir().expect("tempdir");
    let harness = WatcherHarness::open(dir.path(), FolderRule::ImportOnly).await;
    let path = harness.path("burst.wav");
    write_file(&path, &wav_bytes(8, 2_048));
    let snapshot = SourceSnapshot::capture(&path);

    let mut source = ScriptedEventSource::new();
    for _ in 0..50 {
        source.touch(path.clone());
    }
    let report = harness.service.pump(&mut source);
    assert_eq!(report.hints, 50);
    assert_eq!(harness.service.queue().len(), 1, "one path, one candidate");

    // Drain repeatedly: the barrier defers until the window passes, and the
    // candidate is requeued rather than lost.
    let mut imports = 0;
    for _ in 0..6 {
        harness.advance(i64::try_from(QUIESCENCE_MS).unwrap() + 1);
        for outcome in harness.service.drain_due().await.expect("drain") {
            if imported_recording(&outcome) {
                imports += 1;
            }
        }
    }
    assert_eq!(imports, 1, "a burst is one import attempt");
    assert_eq!(recording_count(&harness).await, 1);

    let snapshot_metrics = harness.service.metrics().watcher_snapshot();
    assert_eq!(snapshot_metrics.event_hints, 50);
    assert!(
        snapshot_metrics.coalescing_ratio().unwrap() > 0.9,
        "{snapshot_metrics:?}"
    );
    snapshot.assert_unchanged();
}

// --------------------------------------------- §18.6: shutdown during copy -

#[tokio::test]
async fn a_shutdown_mid_copy_leaves_no_half_committed_recording() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path;
    {
        let harness = WatcherHarness::open(dir.path(), FolderRule::ImportOnly).await;
        path = harness.path("interrupted.wav");
        write_file(&path, &wav_bytes(11, 1_000));
        let outcome = harness.process(&path).await;
        assert!(matches!(outcome, CandidateOutcome::Deferred { .. }));
        assert_eq!(recording_count(&harness).await, 0);
        harness.database.close().await;
    }

    // Restart against the same library and finish the copy.
    let harness = WatcherHarness::open(dir.path(), FolderRule::ImportOnly).await;
    append_file(&path, &wav_bytes(12, 1_000));
    let snapshot = SourceSnapshot::capture(&path);
    let outcome = harness.settle(&path).await;
    assert!(imported_recording(&outcome), "unexpected: {outcome:?}");
    assert_eq!(recording_count(&harness).await, 1);
    assert_eq!(media_asset_count(&harness).await, 1);

    let orphaned: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM import_journal WHERE state NOT IN ('committed','abandoned','quarantined')",
    )
    .fetch_one(harness.database.pool())
    .await
    .expect("journal");
    assert_eq!(orphaned, 0, "no intent may be left dangling");
    snapshot.assert_unchanged();
}

// ------------------------------------------------- §18.6: source deletion --

#[tokio::test]
async fn a_deleted_source_file_marks_the_association_unavailable_and_keeps_everything() {
    let dir = tempfile::tempdir().expect("tempdir");
    let harness = WatcherHarness::open(dir.path(), FolderRule::ImportOnly).await;
    let path = harness.path("deleted.wav");
    write_file(&path, &wav_bytes(13, 2_048));
    let outcome = harness.settle(&path).await;
    assert!(imported_recording(&outcome), "unexpected: {outcome:?}");

    let blobs_before = harness.archive.list_blobs().expect("blobs");
    assert_eq!(blobs_before.len(), 1);

    // The user removes the original. That is their file, not our content.
    std::fs::remove_file(&path).expect("remove");
    harness.advance(1_000);
    let report = harness.service.run_inventory().await.expect("sweep");
    assert_eq!(report.vanished.len(), 1, "{report:?}");

    let items = harness.service.items().await.expect("items");
    let row = items.first().expect("one item");
    assert_eq!(row.state, "unavailable");
    assert!(row.unavailable_since.is_some());

    assert_eq!(recording_count(&harness).await, 1, "the recording survives");
    assert_eq!(
        harness.archive.list_blobs().expect("blobs").len(),
        1,
        "the archived bytes survive"
    );
}

// --------------------------------------------------- §18.6: rename storm ---

#[tokio::test]
async fn a_rename_storm_settles_without_duplicating_or_damaging_anything() {
    let dir = tempfile::tempdir().expect("tempdir");
    let harness = WatcherHarness::open(dir.path(), FolderRule::ImportOnly).await;
    let bytes = wav_bytes(21, 3_000);
    let mut current = harness.path("storm-0.wav");
    write_file(&current, &bytes);

    let mut source = ScriptedEventSource::new();
    for step in 1..25 {
        let next = harness.path(&format!("storm-{step}.wav"));
        std::fs::rename(&current, &next).expect("rename");
        source.push(FsHint::Touched(current.clone()));
        source.push(FsHint::Touched(next.clone()));
        current = next;
    }
    // Stale paths no longer exist and must be dropped, not imported.
    let report = harness.service.pump(&mut source);
    assert!(report.rejected >= 23, "{report:?}");

    let snapshot = SourceSnapshot::capture(&current);
    let mut imports = 0;
    for _ in 0..6 {
        harness.advance(i64::try_from(QUIESCENCE_MS).unwrap() + 1);
        for outcome in harness.service.drain_due().await.expect("drain") {
            if imported_recording(&outcome) {
                imports += 1;
            }
        }
    }
    assert_eq!(imports, 1, "one file, however many names it had");
    assert_eq!(recording_count(&harness).await, 1);
    snapshot.assert_unchanged();
}

// ------------------------------------------------- §18.6: race conditions --

#[tokio::test]
async fn two_concurrent_passes_over_one_file_create_one_recording() {
    let dir = tempfile::tempdir().expect("tempdir");
    let harness = WatcherHarness::open(dir.path(), FolderRule::ImportOnly).await;
    let path = harness.path("raced.wav");
    write_file(&path, &wav_bytes(31, 2_500));
    let snapshot = SourceSnapshot::capture(&path);

    // Cross the quiescence window so both passes see a ready candidate.
    let _ = harness.process(&path).await;
    harness.advance(i64::try_from(QUIESCENCE_MS).unwrap() + 1);

    let (left, right) = tokio::join!(
        harness.service.process_path(&path),
        harness.service.process_path(&path)
    );
    let left = left.expect("left");
    let right = right.expect("right");
    let imports = usize::from(imported_recording(&left)) + usize::from(imported_recording(&right));
    assert!(imports >= 1, "{left:?} / {right:?}");
    assert_eq!(
        recording_count(&harness).await,
        1,
        "a race must not double-import: {left:?} / {right:?}"
    );
    assert_eq!(media_asset_count(&harness).await, 1);
    snapshot.assert_unchanged();
}

#[tokio::test]
async fn a_file_rewritten_while_it_is_hashed_is_rejected_rather_than_half_imported() {
    let dir = tempfile::tempdir().expect("tempdir");
    let harness = WatcherHarness::open(dir.path(), FolderRule::ImportOnly).await;
    let path = harness.path("mutating.wav");
    write_file(&path, &wav_bytes(41, 4_000));

    // Probe once to open the window, then change the file before the probe
    // that would otherwise import it.
    let _ = harness.process(&path).await;
    harness.advance(i64::try_from(QUIESCENCE_MS).unwrap() + 1);
    write_file(&path, &wav_bytes(42, 6_000));
    let snapshot = SourceSnapshot::capture(&path);
    let outcome = harness.process(&path).await;
    assert!(
        matches!(outcome, CandidateOutcome::Deferred { .. }),
        "changed bytes restart the window: {outcome:?}"
    );
    assert_eq!(recording_count(&harness).await, 0);
    snapshot.assert_unchanged();
}

// --------------------------------------------- App. A.4: hostile inputs ----

#[tokio::test]
async fn a_decomposed_unicode_filename_imports_and_keeps_its_name() {
    let dir = tempfile::tempdir().expect("tempdir");
    let harness = WatcherHarness::open(dir.path(), FolderRule::ImportOnly).await;
    // "Réunion" in NFD: the accent is a combining mark.
    let name = "Re\u{301}union e\u{301}quipe.wav";
    let path = harness.path(name);
    write_file(&path, &wav_bytes(51, 2_048));
    let snapshot = SourceSnapshot::capture(&path);

    let outcome = harness.settle(&path).await;
    assert!(imported_recording(&outcome), "unexpected: {outcome:?}");
    assert!(path.exists(), "the original name is untouched");
    snapshot.assert_unchanged();
}

#[tokio::test]
async fn a_windows_reserved_device_name_is_rejected_on_every_platform() {
    let dir = tempfile::tempdir().expect("tempdir");
    let harness = WatcherHarness::open(dir.path(), FolderRule::ImportOnly).await;
    let path = harness.path("CON");
    write_file(&path, &wav_bytes(61, 1_024));
    let snapshot = SourceSnapshot::capture(&path);

    let outcome = harness.process(&path).await;
    assert!(
        matches!(outcome, CandidateOutcome::Rejected { .. }),
        "a reserved device name must be refused: {outcome:?}"
    );
    assert_eq!(recording_count(&harness).await, 0);
    snapshot.assert_unchanged();
}

#[tokio::test]
async fn an_absurdly_long_path_is_rejected_without_touching_the_disk() {
    let dir = tempfile::tempdir().expect("tempdir");
    let harness = WatcherHarness::open(dir.path(), FolderRule::ImportOnly).await;
    let path = harness.path(&format!("{}.wav", "n".repeat(8_000)));
    let outcome = harness.process(&path).await;
    assert!(
        matches!(outcome, CandidateOutcome::Rejected { .. }),
        "{outcome:?}"
    );
    assert_eq!(recording_count(&harness).await, 0);
}

#[cfg(unix)]
#[tokio::test]
async fn a_symlink_loop_is_bounded_and_never_imported() {
    let dir = tempfile::tempdir().expect("tempdir");
    let harness = WatcherHarness::open_with(
        dir.path(),
        FolderRule::ImportOnly,
        Arc::new(plaudon_source_folder::fsmeta::OsDiskSpaceProbe),
        true,
    )
    .await;
    let left = harness.root.join("loop-a");
    let right = harness.root.join("loop-b");
    std::os::unix::fs::symlink(&right, &left).expect("symlink a");
    std::os::unix::fs::symlink(&left, &right).expect("symlink b");
    let real = harness.path("real.wav");
    write_file(&real, &wav_bytes(71, 2_048));
    let snapshot = SourceSnapshot::capture(&real);

    let outcome = harness.process(&left).await;
    assert!(
        matches!(outcome, CandidateOutcome::Rejected { .. }),
        "{outcome:?}"
    );

    // The sweep walks past the loop and still finds the real file.
    let report = harness.service.run_inventory().await.expect("sweep");
    assert!(report.is_complete(), "{report:?}");
    let mut imports = 0;
    for _ in 0..6 {
        harness.advance(i64::try_from(QUIESCENCE_MS).unwrap() + 1);
        for outcome in harness.service.drain_due().await.expect("drain") {
            if imported_recording(&outcome) {
                imports += 1;
            }
        }
    }
    assert_eq!(imports, 1);
    snapshot.assert_unchanged();
    // `exists()` follows links, and these two resolve to each other; the
    // links themselves must still be there.
    assert!(
        std::fs::symlink_metadata(&left).is_ok() && std::fs::symlink_metadata(&right).is_ok(),
        "no destructive cleanup"
    );
}

#[tokio::test]
async fn a_file_that_is_not_a_media_container_is_refused_however_it_is_named() {
    let dir = tempfile::tempdir().expect("tempdir");
    let harness = WatcherHarness::open(dir.path(), FolderRule::ImportOnly).await;
    let path = harness.path("lying.wav");
    write_file(&path, b"this is a text file wearing a wav extension\n");
    let snapshot = SourceSnapshot::capture(&path);

    let outcome = harness.settle(&path).await;
    match &outcome {
        CandidateOutcome::Deferred { reason, .. } => {
            assert_eq!(reason, &DeferReason::UnrecognisedContainer);
        }
        other => panic!("extension is not evidence: {other:?}"),
    }
    assert_eq!(recording_count(&harness).await, 0);
    snapshot.assert_unchanged();
}

#[tokio::test]
async fn a_full_volume_defers_instead_of_staging_a_partial_copy() {
    let dir = tempfile::tempdir().expect("tempdir");
    let harness = WatcherHarness::open_with(
        dir.path(),
        FolderRule::ImportOnly,
        Arc::new(FixedDiskSpaceProbe::with_available(16)),
        false,
    )
    .await;
    let path = harness.path("big.wav");
    write_file(&path, &wav_bytes(81, 4_096));
    let snapshot = SourceSnapshot::capture(&path);

    let outcome = harness.settle(&path).await;
    match &outcome {
        CandidateOutcome::Deferred { reason, .. } => {
            assert!(
                matches!(reason, DeferReason::InsufficientFreeSpace { .. }),
                "{reason:?}"
            );
        }
        other => panic!("expected a deferral: {other:?}"),
    }
    assert_eq!(recording_count(&harness).await, 0);
    snapshot.assert_unchanged();
}

// ----------------------------------------- §4.3(7): per-folder rules -------

#[tokio::test]
async fn import_and_queue_local_stt_queues_local_work_only() {
    let dir = tempfile::tempdir().expect("tempdir");
    let harness = WatcherHarness::open(dir.path(), FolderRule::ImportAndQueueLocalStt).await;
    let sink = Arc::new(QueueingLocalWorkSink::new());
    let service = harness.service.clone().with_local_work(sink.clone());

    let path = harness.path("queued.wav");
    write_file(&path, &wav_bytes(91, 2_048));
    let snapshot = SourceSnapshot::capture(&path);

    let mut outcome = service.process_path(&path).await.expect("process");
    for _ in 0..4 {
        if !matches!(outcome, CandidateOutcome::Deferred { .. }) {
            break;
        }
        harness.advance(i64::try_from(QUIESCENCE_MS).unwrap() + 1);
        outcome = service.process_path(&path).await.expect("process");
    }
    match outcome {
        CandidateOutcome::Imported {
            queued_local_stt, ..
        } => assert!(queued_local_stt),
        other => panic!("expected an import, got {other:?}"),
    }
    assert_eq!(sink.take().len(), 1);
    snapshot.assert_unchanged();
}

#[tokio::test]
async fn ask_before_cloud_refuses_background_dispatch_without_a_grant() {
    let dir = tempfile::tempdir().expect("tempdir");
    let harness = WatcherHarness::open(dir.path(), FolderRule::AskBeforeCloud).await;
    assert!(!harness.service.permits_unattended_cloud());

    let route = ProviderRoute::new("openrouter", "some/model").with_downstream_provider("acme");
    let request = DispatchRequest::new(
        ConsentPurpose::Transcription,
        ContentType::AudioBytes,
        route,
        "recording-1",
        "v1",
    )
    .with_resolved_retention(RetentionPosture::ZeroRetention);

    // No grants at all.
    let refused = harness.service.authorize_background_cloud_dispatch(
        &OutboundPolicy::local_only(),
        &[],
        &request,
        0,
    );
    assert!(refused.is_err(), "no grant means no dispatch");

    // A grant for a different route is not this grant.
    let other_route =
        ProviderRoute::new("openrouter", "other/model").with_downstream_provider("acme");
    let grant = ConsentGrant::new(
        "grant-1",
        GrantScope::ModelPolicy {
            route: other_route.clone(),
        },
        ConsentPurpose::Transcription,
        "v1",
        0,
    )
    .with_content_type(ContentType::AudioBytes)
    .with_route(other_route);
    let still_refused = harness.service.authorize_background_cloud_dispatch(
        &OutboundPolicy::local_only(),
        std::slice::from_ref(&grant),
        &request,
        0,
    );
    assert!(still_refused.is_err(), "a mismatched grant is not consent");
}

// ---------------------------------- §4.3(2)/(6), §17.4: inventory recovery -

#[tokio::test]
async fn changes_the_watcher_dropped_are_recovered_by_the_next_sweep() {
    let dir = tempfile::tempdir().expect("tempdir");
    let harness = WatcherHarness::open(dir.path(), FolderRule::ImportOnly).await;
    let path = harness.path("missed.wav");
    write_file(&path, &wav_bytes(101, 2_048));
    let snapshot = SourceSnapshot::capture(&path);

    // The platform watcher reports that it lost events and nothing else.
    let mut source = ScriptedEventSource::new();
    source.push(FsHint::Rescan);
    let report = harness.service.pump(&mut source);
    assert!(report.rescan_requested);
    assert_eq!(harness.service.queue().len(), 0, "no usable hint arrived");

    let sweep = harness.service.run_inventory().await.expect("sweep");
    assert_eq!(sweep.enqueued, 1, "{sweep:?}");

    let mut imports = 0;
    for _ in 0..6 {
        harness.advance(i64::try_from(QUIESCENCE_MS).unwrap() + 1);
        for outcome in harness.service.drain_due().await.expect("drain") {
            if imported_recording(&outcome) {
                imports += 1;
            }
        }
    }
    assert_eq!(imports, 1);

    let metrics = harness.service.metrics().watcher_snapshot();
    assert_eq!(metrics.changes_missed, 1);
    assert_eq!(metrics.changes_recovered_by_inventory, 1);
    assert_eq!(
        metrics.missed_change_recovery_fraction(),
        Some(1.0),
        "{metrics:?}"
    );
    snapshot.assert_unchanged();
}

#[tokio::test]
async fn the_inventory_sweep_respects_its_entry_cap() {
    let dir = tempfile::tempdir().expect("tempdir");
    let harness = WatcherHarness::open(dir.path(), FolderRule::ImportOnly).await;
    for index in 0..12 {
        write_file(&harness.path(&format!("many-{index}.wav")), &wav_bytes(1, 64));
    }
    let queue = CoalescingQueue::new(QUIESCENCE_MS, 64);
    let mut capped = harness.service.config().clone();
    capped = capped.with_caps(plaudon_source_folder::config::FolderCaps {
        max_entries: 4,
        ..*harness.service.config().caps()
    });
    let report = plaudon_source_folder::inventory::sweep(
        &capped,
        &queue,
        &BTreeSet::new(),
        &harness.clock,
    );
    assert!(report.truncated_by_entries, "{report:?}");
    assert!(report.scanned <= 4, "{report:?}");
    assert!(
        !report.is_complete(),
        "a truncated sweep may not conclude anything vanished"
    );
    assert!(report.vanished.is_empty());
}

#[tokio::test]
async fn the_whole_source_tree_is_byte_identical_after_a_full_sync() {
    let dir = tempfile::tempdir().expect("tempdir");
    let harness = WatcherHarness::open_with(
        dir.path(),
        FolderRule::ImportOnly,
        Arc::new(plaudon_source_folder::fsmeta::OsDiskSpaceProbe),
        true,
    )
    .await;
    std::fs::create_dir_all(harness.root.join("nested/deeper")).expect("mkdir");
    let paths: Vec<PathBuf> = vec![
        harness.path("one.wav"),
        harness.root.join("nested/two.wav"),
        harness.root.join("nested/deeper/three.wav"),
    ];
    for (index, path) in paths.iter().enumerate() {
        write_file(path, &wav_bytes(u8::try_from(index).unwrap() + 1, 1_500));
    }
    let before = snapshot_tree(&harness.root);

    for _ in 0..6 {
        harness.advance(i64::try_from(QUIESCENCE_MS).unwrap() + 1);
        let _ = harness.service.sync().await.expect("sync");
    }
    assert_eq!(recording_count(&harness).await, 3);

    assert_tree_unchanged(&before);
    let metrics = harness.service.metrics().watcher_snapshot();
    assert!(metrics.source_files_preserved(), "{metrics:?}");
    assert_eq!(metrics.source_preservation_failures, 0);
    assert!(metrics.import_latency.count >= 3);

    // The archived bytes equal the source bytes, which is the other half of
    // "the source file is unharmed".
    for path in &paths {
        let source_digest = Sha256Digest::of(&read_file(path)).to_hex();
        let blobs = harness.archive.list_blobs().expect("blobs");
        assert!(
            blobs.iter().any(|(relative, _)| relative.contains(
                &source_digest[..2]
            ) && relative.contains(&source_digest)),
            "archive is missing {source_digest}"
        );
    }
}

// --------------------------------------------------------- property tests --

mod properties {
    use super::*;
    use proptest::prelude::*;

    /// An event sequence drawn from the things a watcher can actually report.
    #[derive(Debug, Clone, Copy)]
    enum Step {
        Touch,
        Append,
        Advance,
        Rescan,
    }

    fn steps() -> impl Strategy<Value = Vec<Step>> {
        proptest::collection::vec(
            prop_oneof![
                Just(Step::Touch),
                Just(Step::Append),
                Just(Step::Advance),
                Just(Step::Rescan),
            ],
            1..18,
        )
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(12))]

        /// App. A.4: no event sequence imports a file that is still changing.
        #[test]
        fn no_event_sequence_imports_before_the_barrier_passes(script in steps()) {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("runtime");
            runtime.block_on(async move {
                let dir = tempfile::tempdir().expect("tempdir");
                let harness = WatcherHarness::open(dir.path(), FolderRule::ImportOnly).await;
                let path = harness.path("fuzzed.wav");
                write_file(&path, &wav_bytes(5, 512));
                let mut last_write_ms = harness.now_ms();

                for step in script {
                    match step {
                        Step::Touch => {
                            let mut source = ScriptedEventSource::new();
                            source.touch(path.clone());
                            harness.service.pump(&mut source);
                        }
                        Step::Append => {
                            append_file(&path, &[0_u8; 64]);
                            last_write_ms = harness.now_ms();
                        }
                        Step::Advance => {
                            harness.advance(i64::try_from(QUIESCENCE_MS).unwrap() + 1);
                        }
                        Step::Rescan => {
                            let mut source = ScriptedEventSource::new();
                            source.push(FsHint::Rescan);
                            harness.service.pump(&mut source);
                        }
                    }
                    let outcomes = harness.service.drain_due().await.expect("drain");
                    for outcome in outcomes {
                        if matches!(outcome, CandidateOutcome::Imported { .. }) {
                            let quiesced = harness.now_ms() - last_write_ms
                                >= i64::try_from(QUIESCENCE_MS).unwrap();
                            prop_assert!(
                                quiesced,
                                "imported {}ms after the last write",
                                harness.now_ms() - last_write_ms
                            );
                        }
                    }
                }
                SourceSnapshot::capture(&path).assert_unchanged();
                Ok(())
            })?;
        }

        /// Any number of hints for one path is one pending candidate.
        #[test]
        fn hints_for_one_path_never_exceed_one_pending_candidate(hints in 1_usize..200) {
            let queue = CoalescingQueue::new(10, 64);
            for tick in 0..hints {
                let outcome = queue.offer("/tmp/one.wav", i64::try_from(tick).unwrap());
                prop_assert!(matches!(
                    outcome,
                    OfferOutcome::Queued | OfferOutcome::Coalesced
                ));
            }
            prop_assert_eq!(queue.len(), 1);
        }
    }
}

// ------------------------------------------------------- barrier contract --

#[tokio::test]
async fn the_barrier_reports_every_check_it_applies() {
    let dir = tempfile::tempdir().expect("tempdir");
    let harness = WatcherHarness::open(dir.path(), FolderRule::ImportOnly).await;
    let path = harness.path("contract.wav");
    write_file(&path, &wav_bytes(3, 1_024));

    let first = harness.service.barrier().probe(&path, harness.now_ms());
    assert!(matches!(
        first,
        BarrierOutcome::Defer(DeferReason::AwaitingQuiescence { .. })
    ));
    harness.advance(i64::try_from(QUIESCENCE_MS).unwrap() + 1);
    let second = harness.service.barrier().probe(&path, harness.now_ms());
    assert!(second.is_ready(), "{second:?}");
    assert!(
        harness.service.barrier().policy().enforces_quiescence(),
        "production policy must enforce quiescence"
    );
}

// --------------------------------------- §18.6A: mutation sanity ----------

/// The suite is only worth anything if it fails against a broken barrier.
///
/// This installs a barrier whose quiescence gate has been removed — the one
/// deliberate regression the `test-harness` feature can express — and asserts
/// that the "late writes" scenario, which passes against the real
/// implementation, now imports a half-written file. If this test ever starts
/// failing, the fixtures above have stopped detecting the regression they
/// exist to detect.
#[tokio::test]
async fn the_fixtures_detect_a_barrier_with_its_quiescence_gate_removed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let weakened = BarrierPolicy::strict(QUIESCENCE_MS, 2, 8 * 1024 * 1024 * 1024, 1024)
        .skipping_quiescence();
    assert!(!weakened.enforces_quiescence());

    let harness =
        WatcherHarness::open_with_barrier(dir.path(), FolderRule::ImportOnly, weakened).await;
    let path = harness.path("late.wav");
    let full = wav_bytes(7, 4_000);
    write_file(&path, &full[..1_500]);

    // Against the real barrier this is a deferral (see the first test in this
    // file). Against the regressed one it imports a partial file.
    let outcome = harness.process(&path).await;
    assert!(
        imported_recording(&outcome),
        "the regression is supposed to import too early; if this fails the \
         mutation check no longer proves anything: {outcome:?}"
    );
    match outcome {
        CandidateOutcome::Imported { content_sha256, .. } => assert_ne!(
            content_sha256,
            Sha256Digest::of(&full).to_hex(),
            "the regressed barrier imported the complete file by luck"
        ),
        other => panic!("expected an import, got {other:?}"),
    }

    // Even a regressed barrier must not damage the source file.
    SourceSnapshot::capture(&path).assert_unchanged();
}
