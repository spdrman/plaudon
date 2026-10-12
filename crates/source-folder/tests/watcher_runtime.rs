//! Tests that need the real platform watcher or the test-harness seam.
//!
//! The deterministic §18.6 fixture cluster lives in
//! `tests/integration/tests/k2_watched_folder.rs`; what cannot live there is
//! anything that exercises `notify` itself, because a real filesystem event
//! has real latency. Those tests are bounded: they poll until the event
//! arrives or a deadline passes, so they finish as fast as the platform
//! allows and fail rather than hang.

use std::fs;
use std::io::Write;
use std::path::Path;
use std::time::{Duration, Instant};

use plaudon_source_folder::barrier::{BarrierOutcome, BarrierPolicy, CompletionBarrier, DeferReason};
use plaudon_source_folder::clock::{Clock, ManualClock};
use plaudon_source_folder::config::{FolderRule, WatchedFolderConfig};
use plaudon_source_folder::queue::CoalescingQueue;
use plaudon_source_folder::watcher::{pump, FsEventSource, FsHint, NotifyWatcher};

const GIGABYTE: u64 = 1024 * 1024 * 1024;

fn wav(seed: u8, payload: usize) -> Vec<u8> {
    let data: Vec<u8> = (0..payload)
        .map(|index| seed.wrapping_add(u8::try_from(index % 251).unwrap_or(0)))
        .collect();
    let data_len = u32::try_from(data.len()).unwrap_or(0);
    let mut out = Vec::new();
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16_u32.to_le_bytes());
    out.extend_from_slice(&1_u16.to_le_bytes());
    out.extend_from_slice(&1_u16.to_le_bytes());
    out.extend_from_slice(&16_000_u32.to_le_bytes());
    out.extend_from_slice(&32_000_u32.to_le_bytes());
    out.extend_from_slice(&2_u16.to_le_bytes());
    out.extend_from_slice(&16_u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    out.extend_from_slice(&data);
    out
}

fn write(path: &Path, bytes: &[u8]) {
    let mut file = fs::File::create(path).expect("create");
    file.write_all(bytes).expect("write");
    file.sync_all().expect("sync");
}

#[test]
fn the_platform_watcher_reports_a_created_file_as_a_hint() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = WatchedFolderConfig::new(dir.path(), FolderRule::ImportOnly)
        .expect("config")
        .with_quiescence_ms(10);
    let mut watcher = match NotifyWatcher::start(&config) {
        Ok(watcher) => watcher,
        Err(error) => {
            // A sandbox without filesystem notifications is a platform fact,
            // not a product failure; the inventory sweep is the guarantee.
            eprintln!("skipping: platform watcher unavailable: {error}");
            return;
        }
    };

    let path = config.root().join("created.wav");
    write(&path, &wav(1, 512));

    let queue = CoalescingQueue::new(0, 64);
    let clock = ManualClock::starting_at(0);
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut saw_hint = false;
    while Instant::now() < deadline && !saw_hint {
        let report = pump(&mut watcher, &config, &queue, &clock);
        saw_hint = report.hints > 0 || report.rescan_requested;
        if !saw_hint {
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    assert!(
        saw_hint,
        "the platform watcher produced no hint for a created file"
    );
    // A hint is only ever a hint: the queue holds a candidate, nothing more.
    assert!(queue.len() <= 1);
    assert!(path.exists(), "watching must not disturb the folder");
}

#[test]
fn a_watcher_error_is_reported_as_a_rescan_rather_than_silence() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = WatchedFolderConfig::new(dir.path(), FolderRule::ImportOnly).expect("config");
    let queue = CoalescingQueue::new(0, 64);
    let clock = ManualClock::starting_at(0);

    #[derive(Debug)]
    struct Failing(bool);
    impl FsEventSource for Failing {
        fn drain_hints(&mut self) -> Vec<FsHint> {
            if self.0 {
                return Vec::new();
            }
            self.0 = true;
            vec![FsHint::Rescan]
        }
    }

    let mut source = Failing(false);
    let report = pump(&mut source, &config, &queue, &clock);
    assert!(report.rescan_requested);
    assert_eq!(
        queue.dropped_hints(),
        1,
        "a lost-event condition must be recoverable work, not silence"
    );
}

#[test]
fn a_non_recursive_folder_ignores_candidates_in_subdirectories() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = WatchedFolderConfig::new(dir.path(), FolderRule::ImportOnly).expect("config");
    let nested = config.root().join("deeper");
    fs::create_dir_all(&nested).expect("mkdir");
    let inside = nested.join("ignored.wav");
    write(&inside, &wav(2, 256));
    let top = config.root().join("seen.wav");
    write(&top, &wav(3, 256));

    assert!(config.validate_candidate(&top).is_ok());
    assert!(config.validate_candidate(&inside).is_err());

    let listed = plaudon_source_folder::inventory::list_candidates(&config);
    assert_eq!(listed.len(), 1, "{listed:?}");
}

#[test]
fn a_file_is_not_a_watchable_root() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("not-a-folder.wav");
    write(&path, &wav(4, 64));
    assert!(WatchedFolderConfig::new(&path, FolderRule::ImportOnly).is_err());
}

/// Crate-local mutation sanity: the quiescence seam must actually change
/// behaviour, or the integration-level mutation check is testing nothing.
#[test]
fn removing_the_quiescence_gate_changes_the_barrier_answer() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("fresh.wav");
    write(&path, &wav(5, 1_024));

    let strict = BarrierPolicy::strict(5_000, 2, GIGABYTE, 1_024);
    let strict_barrier = CompletionBarrier::new(strict);
    assert!(matches!(
        strict_barrier.probe(&path, 0),
        BarrierOutcome::Defer(DeferReason::AwaitingQuiescence { .. })
    ));

    let weakened = strict.skipping_quiescence();
    let weak_barrier = CompletionBarrier::new(weakened);
    assert!(
        weak_barrier.probe(&path, 0).is_ready(),
        "the seam must remove the gate it claims to remove"
    );
    assert!(path.exists());
}

#[test]
fn the_barrier_never_modifies_the_file_it_probes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("untouched.wav");
    let bytes = wav(6, 2_048);
    write(&path, &bytes);
    let before = fs::read(&path).expect("read");

    let barrier = CompletionBarrier::new(BarrierPolicy::strict(10, 2, GIGABYTE, 1_024));
    let clock = ManualClock::starting_at(0);
    for _ in 0..5 {
        let _ = barrier.probe(&path, clock.now_ms());
        clock.advance_ms(20);
    }
    assert_eq!(fs::read(&path).expect("read"), before);
}
