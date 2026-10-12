//! Watched-folder and source-reconciliation fixture harness (§18.6, §18.6A).
//!
//! Everything here is hermetic and deterministic: a real SQLite library on a
//! caller-supplied temporary directory, a real media archive, a manual clock
//! so no test ever sleeps through a quiescence window, and scripted probes for
//! the two conditions a filesystem cannot be asked to produce on demand — a
//! cloud placeholder and a full volume.
//!
//! The harness owns one assertion the whole §18.6 watcher cluster depends on:
//! [`SourceSnapshot::assert_unchanged`]. §4.3(4) and §22(11) require that a
//! source file is never renamed, moved, truncated or deleted, so every
//! scenario takes a snapshot before it runs and proves byte identity after.
//!
//! The caller supplies the base directory (normally a `tempfile::TempDir`),
//! which keeps this module free of test-only dependencies.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use plaudon_domain::media::Sha256Digest;
use plaudon_persistence::archive::MediaArchive;
use plaudon_persistence::migrations::MigrationRunner;
use plaudon_persistence::Database;
use plaudon_source_folder::barrier::BarrierPolicy;
use plaudon_source_folder::clock::{Clock, ManualClock};
use plaudon_source_folder::config::{FolderCaps, FolderRule, WatchedFolderConfig};
use plaudon_source_folder::fsmeta::{DiskSpaceProbe, OsDiskSpaceProbe};
use plaudon_source_folder::placeholder::ScriptedPlaceholderProbe;
use plaudon_source_folder::service::{CandidateOutcome, WatchedFolderService};
use plaudon_source_folder::CompletionBarrier;

/// Quiescence window used by every fixture. Short, because the clock is
/// manual: the window is crossed by arithmetic, never by waiting.
pub const QUIESCENCE_MS: u64 = 50;

/// Consecutive unchanged probes the fixtures require.
pub const STABLE_PROBES: u32 = 2;

/// A minimal but genuinely sniffable RIFF/WAVE file.
///
/// The watcher's media sniff must recognise the container, so fixtures cannot
/// use arbitrary bytes; `seed` varies the payload so two fixtures can produce
/// either identical or deliberately different content.
#[must_use]
pub fn wav_bytes(seed: u8, payload_len: usize) -> Vec<u8> {
    let payload: Vec<u8> = (0..payload_len)
        .map(|index| seed.wrapping_add(u8::try_from(index % 251).unwrap_or(0)))
        .collect();
    let data_len = u32::try_from(payload.len()).unwrap_or(0);
    let riff_len = 36 + data_len;

    let mut out = Vec::with_capacity(44 + payload.len());
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&riff_len.to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16_u32.to_le_bytes());
    out.extend_from_slice(&1_u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1_u16.to_le_bytes()); // mono
    out.extend_from_slice(&16_000_u32.to_le_bytes());
    out.extend_from_slice(&32_000_u32.to_le_bytes());
    out.extend_from_slice(&2_u16.to_le_bytes());
    out.extend_from_slice(&16_u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    out.extend_from_slice(&payload);
    out
}

/// Everything about a source file that an import must not change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceSnapshot {
    /// The file the snapshot describes.
    pub path: PathBuf,
    /// Length in bytes.
    pub len: u64,
    /// SHA-256 of the contents, lowercase hex.
    pub sha256: String,
    /// Whether the file existed at all.
    pub exists: bool,
}

impl SourceSnapshot {
    /// Capture the current state of a path.
    #[must_use]
    pub fn capture(path: &Path) -> Self {
        let bytes = fs::read(path).ok();
        Self {
            path: path.to_path_buf(),
            len: bytes.as_ref().map_or(0, |b| b.len() as u64),
            sha256: bytes
                .as_ref()
                .map_or_else(String::new, |b| Sha256Digest::of(b).to_hex()),
            exists: bytes.is_some(),
        }
    }

    /// Prove the file is still exactly as it was.
    ///
    /// # Panics
    ///
    /// Panics with a specific message when the file was removed, renamed,
    /// truncated or rewritten — the four things §4.3(4) forbids.
    pub fn assert_unchanged(&self) {
        let now = Self::capture(&self.path);
        assert!(
            now.exists,
            "source file {} was removed or renamed; §4.3(4) forbids it",
            self.path.display()
        );
        assert_eq!(
            now.len,
            self.len,
            "source file {} changed length {} -> {}",
            self.path.display(),
            self.len,
            now.len
        );
        assert_eq!(
            now.sha256,
            self.sha256,
            "source file {} is no longer byte-identical",
            self.path.display()
        );
    }
}

/// Snapshot every regular file under a directory, recursively.
#[must_use]
pub fn snapshot_tree(root: &Path) -> Vec<SourceSnapshot> {
    let mut out = Vec::new();
    collect(root, &mut out);
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

fn collect(dir: &Path, out: &mut Vec<SourceSnapshot>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        match entry.file_type() {
            Ok(file_type) if file_type.is_dir() => collect(&path, out),
            Ok(file_type) if file_type.is_file() => out.push(SourceSnapshot::capture(&path)),
            _ => {}
        }
    }
}

/// Prove that every file in a tree is unchanged, and that none vanished.
///
/// # Panics
///
/// Panics naming the first file that differs.
pub fn assert_tree_unchanged(before: &[SourceSnapshot]) {
    for snapshot in before {
        snapshot.assert_unchanged();
    }
}

/// Write a whole file in one go.
///
/// # Panics
///
/// Panics when the write fails, which in a fixture is a broken test.
pub fn write_file(path: &Path, bytes: &[u8]) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent");
    }
    let mut file = File::create(path).expect("create file");
    file.write_all(bytes).expect("write");
    file.sync_all().expect("sync");
}

/// Append to an existing file, simulating a writer that is not finished.
///
/// # Panics
///
/// Panics when the append fails.
pub fn append_file(path: &Path, bytes: &[u8]) {
    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(path)
        .expect("open for append");
    file.write_all(bytes).expect("append");
    file.sync_all().expect("sync");
}

/// Publish a file the way a careful writer does: write a sibling, then rename
/// it into place. The visible path appears complete the instant it exists.
///
/// # Panics
///
/// Panics when the write or rename fails.
pub fn write_by_atomic_rename(path: &Path, bytes: &[u8]) {
    let staging = path.with_extension("partial-write");
    write_file(&staging, bytes);
    fs::rename(&staging, path).expect("rename into place");
}

/// Read a file back, for tests that compare archived bytes with source bytes.
///
/// # Panics
///
/// Panics when the file cannot be read.
#[must_use]
pub fn read_file(path: &Path) -> Vec<u8> {
    let mut bytes = Vec::new();
    File::open(path)
        .expect("open")
        .read_to_end(&mut bytes)
        .expect("read");
    bytes
}

/// A fully wired watched folder over a temporary library.
#[derive(Debug)]
pub struct WatcherHarness {
    /// The watched folder root.
    pub root: PathBuf,
    /// The open library database.
    pub database: Database,
    /// The media archive the importer commits into.
    pub archive: MediaArchive,
    /// The clock every component reads.
    pub clock: ManualClock,
    /// The placeholder probe, scriptable per path.
    pub placeholder: Arc<ScriptedPlaceholderProbe>,
    /// The service under test.
    pub service: WatchedFolderService,
}

impl WatcherHarness {
    /// Build a harness under `base`, which the caller owns and cleans up.
    ///
    /// # Panics
    ///
    /// Panics when the temporary library cannot be created, which is a broken
    /// fixture rather than a product failure.
    pub async fn open(base: &Path, rule: FolderRule) -> Self {
        Self::open_with(base, rule, Arc::new(OsDiskSpaceProbe), false).await
    }

    /// Build a harness with an injected free-space probe and an explicit
    /// recursion setting.
    ///
    /// # Panics
    ///
    /// Panics when the temporary library cannot be created.
    pub async fn open_with(
        base: &Path,
        rule: FolderRule,
        space: Arc<dyn DiskSpaceProbe>,
        recursive: bool,
    ) -> Self {
        let root = base.join("source");
        fs::create_dir_all(&root).expect("create source root");
        let database = Database::open(base.join("app.sqlite"))
            .await
            .expect("open database");
        MigrationRunner::new()
            .run(&database)
            .await
            .expect("migrate");
        let archive = MediaArchive::open(base.join("archive")).expect("open archive");

        let config = folder_config(&root, rule, recursive);
        let clock = ManualClock::starting_at(1_700_000_000_000);
        let placeholder = Arc::new(ScriptedPlaceholderProbe::always_local());
        let service = WatchedFolderService::with_probes(
            config,
            database.clone(),
            archive.clone(),
            placeholder.clone(),
            space,
            Arc::new(clock.clone()),
        )
        .await
        .expect("configure watched folder");

        Self {
            // The canonical root, which is what every component sees: on
            // macOS a temporary directory is reached through a symlink, so a
            // fixture that scripted probes against the uncanonical path would
            // be scripting a path the service never uses.
            root: service.config().root().to_path_buf(),
            database,
            archive,
            clock,
            placeholder,
            service,
        }
    }

    /// Build a harness around an explicitly supplied barrier.
    ///
    /// The mutation-sanity fixture uses this to install a deliberately
    /// weakened barrier and prove the suite notices.
    ///
    /// # Panics
    ///
    /// Panics when the temporary library cannot be created.
    pub async fn open_with_barrier(base: &Path, rule: FolderRule, policy: BarrierPolicy) -> Self {
        let root = base.join("source");
        fs::create_dir_all(&root).expect("create source root");
        let database = Database::open(base.join("app.sqlite"))
            .await
            .expect("open database");
        MigrationRunner::new()
            .run(&database)
            .await
            .expect("migrate");
        let archive = MediaArchive::open(base.join("archive")).expect("open archive");

        let clock = ManualClock::starting_at(1_700_000_000_000);
        let placeholder = Arc::new(ScriptedPlaceholderProbe::always_local());
        let barrier = CompletionBarrier::with_probes(
            policy,
            placeholder.clone(),
            Arc::new(OsDiskSpaceProbe),
        );
        let service = WatchedFolderService::with_barrier(
            folder_config(&root, rule, false),
            database.clone(),
            archive.clone(),
            barrier,
            Arc::new(clock.clone()),
        )
        .await
        .expect("configure watched folder");

        Self {
            root: service.config().root().to_path_buf(),
            database,
            archive,
            clock,
            placeholder,
            service,
        }
    }

    /// A path inside the watched folder.
    #[must_use]
    pub fn path(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    /// Move the manual clock forward.
    pub fn advance(&self, ms: i64) {
        self.clock.advance_ms(ms);
    }

    /// The current instant.
    #[must_use]
    pub fn now_ms(&self) -> i64 {
        self.clock.now_ms()
    }

    /// Probe and, if allowed, import one candidate exactly once.
    ///
    /// # Panics
    ///
    /// Panics when the library rejects the operation.
    pub async fn process(&self, path: &Path) -> CandidateOutcome {
        self.service.process_path(path).await.expect("process path")
    }

    /// Probe repeatedly, crossing the quiescence window, until the barrier
    /// either imports the file or gives a reason that will not change.
    ///
    /// Returns the last outcome. Bounded, so a permanently deferred candidate
    /// ends the loop instead of hanging the suite.
    ///
    /// # Panics
    ///
    /// Panics when the library rejects the operation.
    pub async fn settle(&self, path: &Path) -> CandidateOutcome {
        let mut last = self.process(path).await;
        for _ in 0..6 {
            if !matches!(last, CandidateOutcome::Deferred { .. }) {
                return last;
            }
            self.advance(i64::try_from(QUIESCENCE_MS).unwrap_or(0) + 1);
            last = self.process(path).await;
        }
        last
    }
}

/// The folder configuration every fixture uses.
fn folder_config(root: &Path, rule: FolderRule, recursive: bool) -> WatchedFolderConfig {
    WatchedFolderConfig::new(root, rule)
        .expect("validate root")
        .recursive(recursive)
        .with_quiescence_ms(QUIESCENCE_MS)
        .with_required_stable_probes(STABLE_PROBES)
        .with_caps(FolderCaps {
            // A temporary directory does not have the production headroom, and
            // the out-of-space case is exercised with an injected probe.
            free_space_headroom_bytes: 1024,
            inventory_time_budget_ms: 60_000,
            ..FolderCaps::default()
        })
}
