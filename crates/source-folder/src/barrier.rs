//! The completion barrier (§4.3(3), App. A.4).
//!
//! App. A.4 is blunt: *"We will refuse to equate a file creation event with a
//! complete transfer."* A candidate becomes importable only after **all** of
//! the following, in this order:
//!
//! 1. **Quiescence** — size and modification time unchanged across the
//!    configured window for N consecutive probes.
//! 2. **Locality** — a cloud-placeholder probe says the bytes are on this
//!    machine. This runs before any read, because reading a dataless file is
//!    what triggers an unwanted hydration download.
//! 3. **Readability** — the file opens and its first *and last* block are
//!    actually read. A partially written or locked file fails here.
//! 4. **Media sniff** — the leading bytes identify a container we accept.
//! 5. **Disk** — the volume has room for the file plus headroom.
//! 6. **Digest** — a full SHA-256, followed by a re-stat; if size or mtime
//!    moved while we were hashing, the digest is discarded and the candidate
//!    goes back to step 1.
//!
//! Anything else defers. Deferral is always retryable and never destructive.

use std::collections::HashMap;
use std::fs::{File, Metadata};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::Mutex;
use sha2::{Digest, Sha256};

use crate::fsmeta::{mtime_ms, stable_file_id, DiskSpaceProbe, OsDiskSpaceProbe, StableFileId};
use crate::placeholder::{
    OsPlaceholderProbe, PlaceholderProbe, PlaceholderReason, PlaceholderState,
};
use crate::sniff::{sniff, MediaContainer, SNIFF_WINDOW_BYTES};

/// Bytes read from each end of the file during the readability check.
pub const PROBE_BLOCK_BYTES: usize = 4096;

/// Thresholds the barrier enforces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BarrierPolicy {
    quiescence_ms: u64,
    required_stable_probes: u32,
    max_file_bytes: u64,
    free_space_headroom_bytes: u64,
    require_quiescence: bool,
}

impl BarrierPolicy {
    /// The production policy. Every check is enabled.
    #[must_use]
    pub const fn strict(
        quiescence_ms: u64,
        required_stable_probes: u32,
        max_file_bytes: u64,
        free_space_headroom_bytes: u64,
    ) -> Self {
        Self {
            quiescence_ms,
            required_stable_probes,
            max_file_bytes,
            free_space_headroom_bytes,
            require_quiescence: true,
        }
    }

    /// The same policy with the quiescence gate removed.
    ///
    /// This exists so the mutation-sanity suite can prove that the watcher
    /// fixtures genuinely fail when the barrier is regressed (§18.6A). It is
    /// compiled only under the `test-harness` feature, so no production build
    /// contains a way to reach it.
    #[cfg(feature = "test-harness")]
    #[must_use]
    pub const fn skipping_quiescence(mut self) -> Self {
        self.require_quiescence = false;
        self
    }

    /// Whether the quiescence gate is enforced.
    #[must_use]
    pub const fn enforces_quiescence(&self) -> bool {
        self.require_quiescence
    }
}

/// Why a candidate is not importable yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeferReason {
    /// Seen, but the quiescence window has not elapsed yet.
    AwaitingQuiescence {
        /// Consecutive unchanged probes so far.
        stable_probes: u32,
    },
    /// Size or modification time moved; the window restarts.
    StillChanging,
    /// The bytes are not local (§7.10).
    CloudPlaceholder(PlaceholderReason),
    /// The file exists but could not be opened or fully read: locked,
    /// permission revoked, or an unreachable network share.
    Unreadable(String),
    /// The leading bytes are not a container we accept.
    UnrecognisedContainer,
    /// Larger than the configured cap.
    TooLarge {
        /// Observed size.
        size_bytes: u64,
        /// Configured cap.
        cap_bytes: u64,
    },
    /// Not enough free space to stage a copy.
    InsufficientFreeSpace {
        /// Bytes required, including headroom.
        needed_bytes: u64,
        /// Bytes available.
        available_bytes: u64,
    },
    /// The path no longer exists.
    Vanished,
}

impl DeferReason {
    /// Stable, non-secret label for status surfaces.
    #[must_use]
    pub const fn label(&self) -> &'static str {
        match self {
            Self::AwaitingQuiescence { .. } => "awaiting_quiescence",
            Self::StillChanging => "still_changing",
            Self::CloudPlaceholder(_) => "cloud_placeholder",
            Self::Unreadable(_) => "unreadable",
            Self::UnrecognisedContainer => "unrecognised_container",
            Self::TooLarge { .. } => "too_large",
            Self::InsufficientFreeSpace { .. } => "insufficient_free_space",
            Self::Vanished => "vanished",
        }
    }

    /// Whether the candidate is worth probing again.
    ///
    /// Everything except a vanished path is: a locked file is unlocked, a
    /// placeholder hydrates, a disk gets emptied.
    #[must_use]
    pub const fn is_retryable(&self) -> bool {
        !matches!(self, Self::Vanished)
    }
}

/// A candidate that passed every check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadyCandidate {
    /// Canonical path the bytes were read from. Provenance, not identity.
    pub path: PathBuf,
    /// Size at the moment the digest was taken.
    pub size_bytes: u64,
    /// Modification time at that moment.
    pub mtime_ms: i64,
    /// Filesystem identity, when the platform supplies one.
    pub stable_file_id: Option<StableFileId>,
    /// SHA-256 of the whole file, lowercase hex.
    pub content_sha256: String,
    /// Container the leading bytes identified.
    pub container: MediaContainer,
    /// The instant the file actually became complete, i.e. the first probe of
    /// the run of unchanged probes. §17.4 measures import latency from here,
    /// not from the event that happened to wake us.
    pub complete_since_ms: i64,
}

/// The barrier's answer for one probe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BarrierOutcome {
    /// Import may proceed.
    Ready(Box<ReadyCandidate>),
    /// Not yet; try again later.
    Defer(DeferReason),
}

impl BarrierOutcome {
    /// Whether this outcome permits an import.
    #[must_use]
    pub const fn is_ready(&self) -> bool {
        matches!(self, Self::Ready(_))
    }
}

#[derive(Debug, Clone, Copy)]
struct ProbeState {
    size_bytes: u64,
    mtime_ms: i64,
    stable_since_ms: i64,
    stable_probes: u32,
}

/// Decides when a watched-folder candidate is complete.
#[derive(Debug, Clone)]
pub struct CompletionBarrier {
    policy: BarrierPolicy,
    placeholder: Arc<dyn PlaceholderProbe>,
    space: Arc<dyn DiskSpaceProbe>,
    seen: Arc<Mutex<HashMap<PathBuf, ProbeState>>>,
}

impl CompletionBarrier {
    /// A barrier using the real platform probes.
    #[must_use]
    pub fn new(policy: BarrierPolicy) -> Self {
        Self::with_probes(
            policy,
            Arc::new(OsPlaceholderProbe),
            Arc::new(OsDiskSpaceProbe),
        )
    }

    /// A barrier with injected placeholder and free-space probes.
    #[must_use]
    pub fn with_probes(
        policy: BarrierPolicy,
        placeholder: Arc<dyn PlaceholderProbe>,
        space: Arc<dyn DiskSpaceProbe>,
    ) -> Self {
        Self {
            policy,
            placeholder,
            space,
            seen: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// The policy in force.
    #[must_use]
    pub const fn policy(&self) -> &BarrierPolicy {
        &self.policy
    }

    /// Forget what we knew about a path, so the next probe starts a fresh
    /// quiescence run. Used when a path is replaced or imported.
    pub fn forget(&self, path: &Path) {
        self.seen.lock().remove(path);
    }

    /// Probe one candidate.
    ///
    /// Takes no locks on the file and writes nothing: the candidate is opened
    /// read-only and closed again, so a probe can never damage a source file.
    pub fn probe(&self, path: &Path, now_ms: i64) -> BarrierOutcome {
        let metadata = match std::fs::metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                self.forget(path);
                return BarrierOutcome::Defer(DeferReason::Vanished);
            }
            Err(error) => return BarrierOutcome::Defer(DeferReason::Unreadable(error.to_string())),
        };
        if !metadata.is_file() {
            return BarrierOutcome::Defer(DeferReason::Unreadable(
                "not a regular file".to_string(),
            ));
        }

        if let Some(deferral) = self.check_quiescence(path, &metadata, now_ms) {
            return BarrierOutcome::Defer(deferral);
        }

        if let PlaceholderState::Placeholder(reason) = self.placeholder.probe(path, &metadata) {
            return BarrierOutcome::Defer(DeferReason::CloudPlaceholder(reason));
        }

        let size_bytes = metadata.len();
        let header = match read_ends(path, size_bytes) {
            Ok(header) => header,
            Err(error) => return BarrierOutcome::Defer(DeferReason::Unreadable(error.to_string())),
        };
        let Some(container) = sniff(&header) else {
            return BarrierOutcome::Defer(DeferReason::UnrecognisedContainer);
        };

        if size_bytes > self.policy.max_file_bytes {
            return BarrierOutcome::Defer(DeferReason::TooLarge {
                size_bytes,
                cap_bytes: self.policy.max_file_bytes,
            });
        }
        let needed_bytes = size_bytes.saturating_add(self.policy.free_space_headroom_bytes);
        match self.space.available_bytes(path) {
            Ok(available_bytes) if available_bytes < needed_bytes => {
                return BarrierOutcome::Defer(DeferReason::InsufficientFreeSpace {
                    needed_bytes,
                    available_bytes,
                });
            }
            Ok(_) => {}
            Err(error) => return BarrierOutcome::Defer(DeferReason::Unreadable(error.to_string())),
        }

        let digest = match digest_file(path) {
            Ok(digest) => digest,
            Err(error) => return BarrierOutcome::Defer(DeferReason::Unreadable(error.to_string())),
        };

        // The file may have been rewritten while we hashed it. Re-stat and
        // throw the digest away rather than import bytes nobody ever had.
        let Ok(after) = std::fs::metadata(path) else {
            return BarrierOutcome::Defer(DeferReason::Vanished);
        };
        if after.len() != size_bytes || mtime_ms(&after) != mtime_ms(&metadata) {
            self.restart(path, &after, now_ms);
            return BarrierOutcome::Defer(DeferReason::StillChanging);
        }

        let complete_since_ms = self
            .seen
            .lock()
            .get(path)
            .map_or(now_ms, |state| state.stable_since_ms);

        BarrierOutcome::Ready(Box::new(ReadyCandidate {
            path: path.to_path_buf(),
            size_bytes,
            mtime_ms: mtime_ms(&metadata),
            stable_file_id: stable_file_id(path, &metadata),
            content_sha256: digest,
            container,
            complete_since_ms,
        }))
    }

    /// Step 1. Returns a deferral while the file is still settling.
    fn check_quiescence(
        &self,
        path: &Path,
        metadata: &Metadata,
        now_ms: i64,
    ) -> Option<DeferReason> {
        let observed_size = metadata.len();
        let observed_mtime = mtime_ms(metadata);
        let mut seen = self.seen.lock();
        let state = seen.entry(path.to_path_buf()).or_insert(ProbeState {
            size_bytes: observed_size,
            mtime_ms: observed_mtime,
            stable_since_ms: now_ms,
            stable_probes: 0,
        });

        if state.size_bytes != observed_size || state.mtime_ms != observed_mtime {
            state.size_bytes = observed_size;
            state.mtime_ms = observed_mtime;
            state.stable_since_ms = now_ms;
            state.stable_probes = 1;
            return Some(DeferReason::StillChanging);
        }
        state.stable_probes = state.stable_probes.saturating_add(1);

        if !self.policy.require_quiescence {
            return None;
        }
        let elapsed = now_ms.saturating_sub(state.stable_since_ms);
        let settled = elapsed >= i64::try_from(self.policy.quiescence_ms).unwrap_or(i64::MAX)
            && state.stable_probes >= self.policy.required_stable_probes;
        if settled {
            None
        } else {
            Some(DeferReason::AwaitingQuiescence {
                stable_probes: state.stable_probes,
            })
        }
    }

    fn restart(&self, path: &Path, metadata: &Metadata, now_ms: i64) {
        self.seen.lock().insert(
            path.to_path_buf(),
            ProbeState {
                size_bytes: metadata.len(),
                mtime_ms: mtime_ms(metadata),
                stable_since_ms: now_ms,
                stable_probes: 1,
            },
        );
    }
}

/// Read the first and last block. An empty file has no last block and is not
/// a recording, so it is rejected rather than treated as readable.
fn read_ends(path: &Path, size_bytes: u64) -> std::io::Result<Vec<u8>> {
    if size_bytes == 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "file is empty",
        ));
    }
    let mut file = File::open(path)?;
    let head_len = usize::try_from(size_bytes).unwrap_or(PROBE_BLOCK_BYTES).min(PROBE_BLOCK_BYTES);
    let mut head = vec![0_u8; head_len];
    file.read_exact(&mut head)?;
    if head.len() < SNIFF_WINDOW_BYTES && size_bytes > head.len() as u64 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "short read",
        ));
    }

    let tail_len = u64::try_from(PROBE_BLOCK_BYTES).unwrap_or(u64::MAX).min(size_bytes);
    let offset = size_bytes - tail_len;
    file.seek(SeekFrom::Start(offset))?;
    let mut tail = vec![0_u8; usize::try_from(tail_len).unwrap_or(PROBE_BLOCK_BYTES)];
    file.read_exact(&mut tail)?;
    Ok(head)
}

/// Full SHA-256 of a file, streamed.
fn digest_file(path: &Path) -> std::io::Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let digest = hasher.finalize();
    Ok(hex_lower(&digest))
}

/// Lowercase hex, without allocating a `String` per byte.
fn hex_lower(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from(DIGITS[usize::from(byte >> 4)]));
        out.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    out
}
